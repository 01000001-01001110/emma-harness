//! `emma set-provider`, `emma set-model`, `emma api`, `emma model`,
//! `emma init`, `emma config check` — the things that run without a model call,
//! and therefore the things worth reaching for when something is wrong.
//!
//! Everything but `init` writes under `~/.emma/`, and `config check` writes
//! nothing at all. None of them touches the project directory — Emma runs
//! inside repositories, and a file it creates next to someone's code is a file
//! their next `git add .` publishes.
//!
//! `api` and `model` are the pre-provider spellings and are kept as aliases
//! rather than removed. `printf %s "$KEY" | emma api` is an existing contract,
//! and two match arms is a cheap price for not breaking it.
//!
//! [`init`] is the exception, and it is the whole of what it does: it writes
//! `.emma/` in the working directory because that is the file whose absence
//! stops Emma starting. It is a deliberate exception rather than a hole in the
//! rule — the user typed a command whose only effect is to create that
//! directory, so nothing is a surprise, and it refuses outright rather than
//! touching a `.emma/` that is already there.

use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use emma_harness::Harness;
use emma_llm::auth::{self, ApiKey};
use emma_tool_api::Registry;

use crate::settings;

// region: provider and model
// ---------------------------------------------------------------------------
// `emma set-provider`, `emma set-model`, and the two older spellings
//
// One rule governs the order of writes here and it is worth stating before the
// code: **a stored key with no model is a reasonable resting place; a stored
// model with no key is not.** So the key is written first and a model is only
// ever stored beside a provider that has one, or that could have one from the
// environment.
//
// Every one of these is split in two: a public function that finds the home
// directory and reads a key from a terminal, and a private one that takes both
// as arguments. Only the second is testable, and it is where all the behaviour
// is — a test that had to prompt for a key would be a test nobody writes.
// ---------------------------------------------------------------------------

/// Read a key from an argument, a pipe, or a prompt that does not echo it.
///
/// Echoing would put the key on the screen, in a screen recording, and — if it
/// was pasted after the command — in shell history. `rpassword` exists in the
/// dependency list for this one call.
///
/// `-` as the argument means stdin explicitly, so a script can say what it
/// means instead of relying on Emma noticing it has no terminal.
fn read_key(given: Option<String>, prompt: &str) -> Result<String> {
    let raw = match given.as_deref() {
        Some("-") | None if !std::io::stdin().is_terminal() => {
            // Piped in: `printf %s "$KEY" | emma set-provider anthropic`.
            let mut raw = String::new();
            std::io::stdin()
                .read_line(&mut raw)
                .context("reading the key from stdin")?;
            raw
        }
        Some("-") => bail!("`--key -` reads the key from stdin, and stdin is a terminal here"),
        Some(_) => given.expect("matched Some"),
        None => rpassword::prompt_password(prompt).context("reading the key")?,
    };
    let key = raw.trim().to_string();
    if key.is_empty() {
        bail!("no key was given; nothing was written");
    }
    Ok(key)
}

/// `emma set-provider <name>`: store that provider's key and select it.
///
/// The name is checked **before** anything is prompted for. `emma set-provider
/// antropic` must not get as far as asking for a secret, and a key typed into a
/// command that is about to fail is a key the user now has to rotate or retype.
pub fn set_provider(name: &str, key: Option<String>, model: Option<String>) -> Result<()> {
    let kind = emma_llm::kind(name)?;
    let home = auth::home_dir().context(
        "the home directory could not be determined, so there is nowhere to store a key",
    )?;
    let key = read_key(key, &format!("{} API key (not echoed): ", kind.name()))?;
    let mut out = std::io::stdout();
    store_provider(&home, kind, &key, model.as_deref(), &mut out)
}

fn store_provider(
    home: &Path,
    kind: &dyn emma_llm::ProviderKind,
    key: &str,
    model: Option<&str>,
    out: &mut dyn Write,
) -> Result<()> {
    let path = auth::store(home, kind.name(), &ApiKey::new(key))?;
    writeln!(out, "provider  {}  {}", kind.name(), path.display())?;

    let mut settings = settings::load(home);
    settings.provider = Some(kind.name().to_string());
    if let Some(model) = model {
        settings
            .models
            .insert(kind.name().to_string(), model.to_string());
    }
    let path = settings::save(home, &settings)?;
    let (model, source) = match settings.models.get(kind.name()) {
        Some(model) => (model.clone(), "settings.json"),
        None => (kind.default_model().to_string(), "built-in default"),
    };
    writeln!(out, "model     {model}  ({source})  {}", path.display())?;
    if source == "built-in default" {
        // The §2.4 resting place, reached in this build by not naming a model
        // rather than by a list call failing. Either way the user is one
        // command from finished and must be told which one.
        writeln!(
            out,
            "\nNo model was chosen, so {model} is what will run. Choose another with \
             `emma set-model <id>`."
        )?;
    }
    env_note(kind, out)
}

/// Say so when the environment outranks what was just written. Otherwise the
/// next run uses the variable and the user concludes the file did not work —
/// which is an hour, reliably, every time.
fn env_note(kind: &dyn emma_llm::ProviderKind, out: &mut dyn Write) -> Result<()> {
    if std::env::var_os(kind.env_var()).is_some() {
        writeln!(
            out,
            "note: {} is set in this shell and takes precedence over the stored key",
            kind.env_var()
        )?;
    }
    Ok(())
}

/// `emma set-model <id>`: set the model remembered for a provider.
pub fn set_model(model: &str, provider: Option<&str>) -> Result<()> {
    let home = auth::home_dir().context(
        "the home directory could not be determined, so there is nowhere to store a preference",
    )?;
    let mut out = std::io::stdout();
    set_model_at(&home, model, provider, &mut out)
}

fn set_model_at(
    home: &Path,
    model: &str,
    provider: Option<&str>,
    out: &mut dyn Write,
) -> Result<()> {
    // A model belongs to a provider, so there must be one to belong to. The
    // message names the command that fixes it, because "set a provider first"
    // with no next step is where somebody puts the tool down.
    if provider.is_none() && settings::load(home).provider.is_none() {
        bail!(
            "no provider is set, and a model id means nothing without one. Run \
             `emma set-provider <name>` — it stores the key and selects the provider — or \
             name one here with `--provider`."
        );
    }
    let (kind, _) = settings::resolve_kind(provider, None, Some(home))?;
    let path = write_model(home, kind.name(), model)?;
    writeln!(out, "model     {model}  for {}", kind.name())?;
    writeln!(out, "stored    {}", path.display())?;
    // A warning rather than a refusal. Nothing here calls the provider, so a
    // model set before a key is a state that costs one more command to leave
    // and nothing else — and refusing would mean a user with a key in a
    // password manager cannot configure Emma until they open it.
    if auth::resolve(kind, std::env::var(kind.env_var()).ok().as_deref(), home).is_err() {
        writeln!(
            out,
            "note: no API key for {} yet, so this model cannot run. `emma set-provider {}` \
             stores one.",
            kind.name(),
            kind.name()
        )?;
    }
    Ok(())
}

/// The one place a model id is written, so "keyed by provider" is a property of
/// the file rather than a habit of two call sites.
pub(crate) fn write_model(home: &Path, provider: &str, model: &str) -> Result<std::path::PathBuf> {
    let mut settings = settings::load(home);
    settings
        .models
        .insert(provider.to_string(), model.to_string());
    settings::save(home, &settings)
}

/// `emma api [<key>]` — the pre-provider spelling, kept.
///
/// It stores a key for the provider already selected and does **not** become
/// the guided flow: `printf %s "$KEY" | emma api` is a contract, and turning it
/// into something that asks questions would break every script that has one.
pub fn api(given: Option<String>) -> Result<()> {
    let home = auth::home_dir().context(
        "the home directory could not be determined, so there is nowhere to store a key",
    )?;
    let (kind, _) = settings::resolve_kind(None, None, Some(&home))?;
    let key = read_key(given, &format!("{} API key (not echoed): ", kind.name()))?;
    let path = auth::store(&home, kind.name(), &ApiKey::new(key))?;
    println!("stored {} for {}", path.display(), kind.name());
    println!(
        "note: `emma set-provider {}` does this and selects the provider in one step.",
        kind.name()
    );
    env_note(kind, &mut std::io::stdout())
}

/// `emma model` reports; `emma model <name>` sets — for the current provider.
///
/// Bare `emma model` still *reports* rather than doing what bare `set-model`
/// would, which is the one place these two deliberately differ: a habitual
/// `emma model` must keep meaning what it meant.
pub fn model(name: Option<String>) -> Result<()> {
    let home = auth::home_dir().context(
        "the home directory could not be determined, so there is nowhere to store a preference",
    )?;
    let (kind, resolved) = settings::resolve_kind(None, None, Some(&home))?;
    match name {
        Some(name) => {
            let path = write_model(&home, kind.name(), &name)?;
            println!("model set to {name} for {}", kind.name());
            println!("stored {}", path.display());
        }
        None => println!("{}  ({})", resolved.model, resolved.model_source),
    }
    Ok(())
}

// endregion: provider and model

// region: init
// ---------------------------------------------------------------------------
// `emma init`
//
// The second half of a safety fix. Storing an API key used to create a harness
// at `~/.emma/` as a side effect, and closing that turned a silent wrong boot
// into a loud refusal — which is progress, and which left the refusal as the
// first thing a new user meets, naming every path it searched and none of the
// files they should create. This writes one of them.
//
// What it writes is a real harness, not a commented example: a persona that is
// selected, instructions that say something, and nothing to uncomment. A
// template a user has to edit before it works is the same dead end one step
// further in.
// ---------------------------------------------------------------------------

/// The persona `init` writes. Named for what it is rather than for the user or
/// the project, because the name appears in `config.json`, in a directory path
/// and in `config check` output, and a name that has to be changed in three
/// places to be accurate is a name that stays wrong.
const PERSONA: &str = "assistant";

/// One persona, selected. `default_persona` is not optional in practice: a
/// `personas/` directory nothing selects is a boot refusal, so a config file
/// without this line would produce a harness that does not load — which is the
/// failure `init` exists to fix, shipped as a template.
const CONFIG: &str = r#"{
  "default_persona": "assistant",
  "personas": {
    "assistant": {
      "description": "The default persona. Its instructions are in personas/assistant/rules.md."
    }
  }
}
"#;

/// The standing instructions. General rather than Emma-specific, and about how
/// to work rather than about what the project is — the project is the one thing
/// the user knows and Emma does not, so this is the half worth shipping.
const RULES: &str = "\
# Assistant

You are a coding agent working in a terminal, in the directory you were started
in. You hold a goal until it is met.

## How you work

- Prefer evidence to recollection. Read the file before saying what it does,
  and run the check before saying it passes.
- Read before writing. `Read` before `Edit`, and `Edit` over `Write` when the
  file already exists.
- Run the project's own checks after changing it, and report what they actually
  said rather than what they were expected to say.
- A tool that fails is information, not a wall. Read the failure, change the
  approach, continue.
- Ask before doing something you cannot undo.

## What you report

State what you did and what the evidence was — the command you ran and its
output, not a summary of how it went. If something is incomplete, say which
part and why.
";

/// The two agent types `init` writes, as `agents/<name>.md`.
///
/// **Two, and deliberately different in kind.** The reason delegation is
/// configuration here rather than something built in is so it can be measured
/// and changed, and a comparison needs two things to compare: one that can only
/// look, and one that can act. `emma agents` is where the difference shows up.
///
/// The format is Claude Code's, unchanged, because that is the format an
/// existing library of these files is already written in: frontmatter carrying
/// `name`, `description`, `tools` and `model`, and a body that is the prompt.
///
/// Each `description` is written as routing advice rather than as advertising,
/// and roughly half of each says when *not* to use it. That text is what the
/// calling model reads to choose, and on code-centric work the measured
/// direction of the effect is negative — under-delegating costs a paragraph of
/// prompt, over-delegating costs a multiple of every goal.
const EXPLORER: &str = "\
---
name: explorer
description: Read-only search across many files, for when the answer is small and the evidence is large \
— \"which of these forty files still calls the old constructor\", \"where is the retry policy actually \
decided\". It reads and searches; it cannot write, edit or run anything. Do not use it for a question one \
Grep answers, for a file you already have open, or to check your own work.
tools: Read, Glob, Grep
---

You are a search agent. You answer one question by reading, and you cannot
change anything.

- Answer from what you read, and cite it: path and line, every time. A claim
  with no path behind it is worse than saying you could not find out.
- Prefer giving the caller places to look over giving it conclusions. A list of
  paths is something it can check by reading one of them; an explanation is
  something it cannot check at all.
- If the answer is not there, say so plainly and say where you looked. The agent
  that sent you cannot see your search, so \"not in X, Y or Z\" is a useful
  answer and a confident guess is not.
- Do not describe what you would do. Read the files.
";

/// The other half of the comparison: a subagent that can change the tree.
///
/// It gets `Write`, `Edit` and `Bash`, and every one of those still goes through
/// the approval gate on the same keyboard as the parent's. A subagent inherits
/// the gate rather than escaping it, which is exactly why only one runs at a
/// time.
const IMPLEMENTER: &str = "\
---
name: implementer
description: Makes one small, well-specified change and reports what it did — a rename across files, a \
mechanical refactor, a test that already has a name and a shape. It can read, write, edit and run \
commands, under the same approval prompts you get. Do not use it for work that needs a decision, for \
anything under about three tool calls, or to verify your own change — run that check yourself and read \
its exit status.
tools: Read, Glob, Grep, Write, Edit, Bash
---

You make one change and report it.

- Read before writing, and edit rather than rewrite when the file already
  exists.
- Run the project's own checks after changing it, and report what they actually
  printed — the command and its exit status, not a summary of how it went. The
  agent that sent you is shown that exit status separately, so a report which
  disagrees with it is worse than no report.
- Stay inside the brief. If the change turns out to need a decision nobody made,
  stop and say which decision, rather than making it.
- Name every file you changed. Nothing else you did reaches the caller.
";

/// Write a minimal working harness in `cwd`.
///
/// **It refuses if `.emma/` exists, and does not merge.** Merging configuration
/// is how configuration becomes impossible to reason about, and this project
/// already ruled that way once, for `.emma/` against `.claude/` — the loser is
/// ignored entirely rather than combined. A half-written harness beside an
/// existing one would be worse than either.
///
/// The report goes to `out` rather than to `println!` because it is the reason
/// the command exists — a refusal with no next step is where somebody puts the
/// tool down — and a thing that matters that much is a thing a test reads.
pub fn init(cwd: &Path, out: &mut dyn Write) -> Result<()> {
    let root = cwd.join(emma_harness::ROOT_DIR_NAME);
    if root.exists() {
        bail!(
            "{} already exists. `init` will not merge into a harness or overwrite one — \
             edit it, or move it aside and run `init` again.",
            root.display()
        );
    }

    let claude = cwd.join(emma_harness::CLAUDE_DIR_NAME);
    if claude.is_dir() {
        // Said before the files appear, because it is the one case where the
        // right answer might be to stop: Emma reads `.claude/` already, and
        // once `.emma/` exists it wins outright and the other is ignored
        // entirely rather than merged.
        writeln!(
            out,
            "{} is already here and Emma reads it. Writing .emma/ anyway, as asked — note \
             that .emma/ wins outright once it exists, and .claude/ is then ignored rather \
             than merged.\n",
            claude.display()
        )?;
    }

    let persona = root.join("personas").join(PERSONA);
    std::fs::create_dir_all(&persona).with_context(|| format!("creating {}", persona.display()))?;
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).with_context(|| format!("creating {}", agents.display()))?;
    let config = root.join("config.json");
    let rules = persona.join("rules.md");
    let explorer = agents.join("explorer.md");
    let implementer = agents.join("implementer.md");
    std::fs::write(&config, CONFIG).with_context(|| format!("writing {}", config.display()))?;
    std::fs::write(&rules, RULES).with_context(|| format!("writing {}", rules.display()))?;
    std::fs::write(&explorer, EXPLORER)
        .with_context(|| format!("writing {}", explorer.display()))?;
    std::fs::write(&implementer, IMPLEMENTER)
        .with_context(|| format!("writing {}", implementer.display()))?;

    writeln!(out, "created {}", config.display())?;
    writeln!(out, "created {}", rules.display())?;
    writeln!(out, "created {}", explorer.display())?;
    writeln!(out, "created {}", implementer.display())?;
    writeln!(
        out,
        "\nEdit {} to say what this agent should know and how it should work.\n\n\
         Two subagent types are in {}, and the model may hand work to either through the \
         `Delegate` tool. They are ordinary files — edit them, add more, or delete the ones \
         you do not want.\n\n\
         Next:\n  \
         emma config check      load it and print exactly what the model will be told\n  \
         emma agents            what delegation has cost, once you have used some\n  \
         emma \"<goal>\"          state a goal\n\n\
         If `emma` says it cannot find an API key, store one with `emma api`.",
        rules.display(),
        agents.display()
    )?;
    Ok(())
}

// endregion: init

// region: emma agents
// ---------------------------------------------------------------------------
// `emma agents`
//
// The reason the delegation records exist. Published measurements say
// multi-agent *loses* on coding tasks — 49–54% single-agent against 10% and 3%
// on SWE-bench Lite, at nine times the spend — but those measured one
// configuration, and here an agent type is a file a person edits. That makes
// the published verdict a baseline rather than a verdict, and the only thing
// that turns a baseline into an answer is measuring your own.
//
// So: the smallest thing that answers "is this type worth using?". It reads the
// same JSONL a person already greps, it calls no model, and it is deliberately
// not a dashboard. `config check` is the shape it copies.
// ---------------------------------------------------------------------------

/// One agent type's record, summed across every session on this machine.
#[derive(Default)]
struct Tally {
    runs: u64,
    finished: u64,
    tokens: i64,
    elapsed_ms: u64,
    iterations: u64,
    tool_calls: u64,
    files: u64,
    commands: u64,
    failed_commands: u64,
    denied: u64,
    endings: std::collections::BTreeMap<String, u64>,
}

/// What each agent type has cost and produced, across every recorded session.
///
/// **Every number here is the harness's, not a model's.** They come off the
/// `delegation` records the `Delegate` tool writes from each sub-run's own log —
/// the same records the footer is built from — so this answers what happened
/// rather than what was reported.
pub fn agents(session_dir: Option<&Path>, out: &mut dyn Write) -> Result<()> {
    let Some(dir) = session_dir else {
        bail!(
            "no session directory: the home directory could not be determined, so there is \
             nothing recorded to read. Name one with --session-dir."
        );
    };
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }
    // Ids sort by time, and an id does not move when a file is copied — the same
    // ordering `session::locate` relies on.
    files.sort();

    let mut by_agent: std::collections::BTreeMap<String, Tally> = Default::default();
    let mut recent: Vec<(String, String, String, i64, u64, String)> = Vec::new();
    for path in &files {
        let Ok(records) = crate::session::SessionLog::read(path) else {
            continue;
        };
        let session = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        for record in records {
            if record["kind"] != "delegation" {
                continue;
            }
            let agent = record["agent"].as_str().unwrap_or("?").to_string();
            let ending = record["ending"].as_str().unwrap_or("?").to_string();
            let tokens = record["cost_tokens"].as_i64().unwrap_or(0);
            let elapsed = record["elapsed_ms"].as_u64().unwrap_or(0);
            let tally = by_agent.entry(agent.clone()).or_default();
            tally.runs += 1;
            // `answered` counts with `done` for the same reason `main` treats
            // them together: a brief that needed no tools was still answered.
            if ending == "done" || ending == "answered" {
                tally.finished += 1;
            }
            tally.tokens += tokens;
            tally.elapsed_ms += elapsed;
            tally.iterations += record["iterations"].as_u64().unwrap_or(0);
            tally.tool_calls += record["tool_calls"].as_u64().unwrap_or(0);
            tally.files += record["files_read"].as_u64().unwrap_or(0);
            tally.commands += record["commands"].as_u64().unwrap_or(0);
            tally.failed_commands += record["failed_commands"].as_u64().unwrap_or(0);
            tally.denied += record["denied"].as_u64().unwrap_or(0);
            *tally.endings.entry(ending.clone()).or_default() += 1;
            recent.push((
                session.clone(),
                agent,
                ending,
                tokens,
                elapsed,
                one_line(record["task"].as_str().unwrap_or("")),
            ));
        }
    }

    writeln!(out, "sessions       {}", files.len())?;
    writeln!(
        out,
        "delegations    {}",
        by_agent.values().map(|t| t.runs).sum::<u64>()
    )?;
    if by_agent.is_empty() {
        writeln!(
            out,
            "\nNothing has been delegated from this machine yet, so there is nothing to \
             compare. `emma config check` lists the agent types this directory offers."
        )?;
        return Ok(());
    }

    writeln!(
        out,
        "\n{:<24}{:>6}{:>10}{:>12}{:>8}{:>8}{:>8}{:>8}{:>8}",
        "agent", "runs", "finished", "tokens/run", "s/run", "calls", "files", "cmds", "denied"
    )?;
    for (name, t) in &by_agent {
        let per = |n: u64| n as f64 / t.runs as f64;
        writeln!(
            out,
            "{:<24}{:>6}{:>10}{:>12}{:>8.0}{:>8.1}{:>8.1}{:>8.1}{:>8}",
            trim_to(name, 23),
            t.runs,
            format!("{}/{}", t.finished, t.runs),
            t.tokens / t.runs as i64,
            per(t.elapsed_ms) / 1000.0,
            per(t.iterations),
            per(t.files),
            per(t.commands),
            t.denied,
        )?;
    }
    // The endings, spelled out. "finished 6/8" does not say whether the other
    // two ran out of budget or were interrupted, and those are different
    // problems with different fixes — one is a number to raise, the other is
    // not.
    writeln!(out)?;
    for (name, t) in &by_agent {
        let endings: Vec<String> = t
            .endings
            .iter()
            .map(|(ending, n)| format!("{ending} {n}"))
            .collect();
        let mut line = format!("{name}: {}", endings.join(", "));
        if t.failed_commands > 0 {
            line.push_str(&format!(
                ", {} of its {} commands exited non-zero",
                t.failed_commands, t.commands
            ));
        }
        writeln!(out, "  {line}")?;
    }

    writeln!(out, "\nmost recent")?;
    for (session, agent, ending, tokens, elapsed, task) in recent.iter().rev().take(RECENT) {
        writeln!(
            out,
            "  {session}  {agent}  {ending}  {tokens} tokens  {}s  {task}",
            elapsed / 1000
        )?;
    }
    writeln!(
        out,
        "\nEvery number here was recorded by the harness from each sub-run's own log, not \
         reported by the agent. The full traffic is in the same files, under `sub.*` records."
    )?;
    Ok(())
}

/// How many delegations the tail lists. Bounded here rather than at the call
/// site, so the cap is a thing somebody can find and change.
const RECENT: usize = 10;

fn one_line(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    trim_to(&text, 60)
}

fn trim_to(text: &str, n: usize) -> String {
    if text.chars().count() > n {
        format!("{}…", text.chars().take(n - 1).collect::<String>())
    } else {
        text.to_string()
    }
}

// endregion: emma agents

/// What this process actually resolved, as opposed to what `settings.json`
/// says.
///
/// `None` from the CLI path, which has no running session and must keep
/// printing exactly what it printed before. `Some` from `/config`, where the
/// interesting case is a user who has just run `/model`: `settings::resolve_kind`
/// would still return the value on disk, and a report that was wrong about the
/// one thing they had just changed would be worse than no report.
#[derive(Debug, Clone)]
pub struct Live {
    pub provider: String,
    pub model: String,
}

/// Load the harness, apply the tool allowlist, and report — without calling a
/// model, which is the whole point. Everything that can fail at startup fails
/// here, where the message is the only output rather than a preamble to one.
///
/// The report goes to `out` rather than to `println!` so the session can have
/// the same bytes: on the framed path a `println!` lands underneath the
/// viewport, and two renderings of one report is how the two doors drift.
pub fn config_check(
    harness: &Harness,
    tools: &Registry,
    cwd: &Path,
    unavailable: &[String],
    live: Option<Live>,
    out: &mut dyn Write,
) -> Result<()> {
    let snapshot = harness.snapshot();
    writeln!(out, "cwd            {}", cwd.display())?;
    writeln!(
        out,
        "harness        {}",
        snapshot["root"].as_str().unwrap_or("?")
    )?;
    writeln!(
        out,
        "flavor         {}",
        snapshot["flavor"].as_str().unwrap_or("?")
    )?;
    writeln!(
        out,
        "persona        {}",
        snapshot["persona"].as_str().unwrap_or("(none)")
    )?;
    writeln!(
        out,
        "instructions   {} ({} bytes{})",
        harness.instructions_hash(),
        harness.instructions.len(),
        if harness.is_empty() {
            ", empty harness"
        } else {
            ""
        }
    )?;
    writeln!(out, "config         {}", harness.config_hash)?;
    writeln!(out, "tools          {}", tools.names().join(", "))?;
    // The tools that were built and then left out because this machine cannot
    // run them. This is the one command whose job is to answer "why can it not
    // do X", and a capability absent for a fixable reason — no browser, no
    // search key — is exactly the question it is asked.
    for line in unavailable {
        writeln!(out, "               {line}")?;
    }
    writeln!(out, "tool schema    {}", tools.schema_hash())?;
    writeln!(
        out,
        "skills         {}",
        or_none(&harness.skill_names().join(", "))
    )?;
    // The delegation catalogue, and — separately — the files that were found and
    // not offered. A catalogue quietly shorter than the directory is the gap
    // nobody notices until the model cannot find an agent that is plainly there,
    // and this is the command whose job is to answer "why can it not do X".
    let types = harness.agent_types();
    writeln!(
        out,
        "agents         {}",
        or_none(
            &types
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    )?;
    // Said explicitly, because the tools line above does not carry it and a
    // reader would reasonably conclude delegation is off. `Delegate` is built
    // from a provider, and this command deliberately runs without one so that it
    // still answers when the key is the thing that is wrong.
    if !types.is_empty() {
        writeln!(
            out,
            "               offered to the model as the `Delegate` tool, which is registered \
             at run time and so is not in the tools line above"
        )?;
    }
    for note in harness.agent_notes() {
        writeln!(out, "               {note}")?;
    }
    let project = harness.command_names();
    writeln!(
        out,
        "commands       {}",
        or_none(
            &project
                .iter()
                .map(|c| format!("/{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    )?;
    // A project command that a built-in shadows. This is the command whose job
    // is answering "why did it not do X", and "because Emma has a command of
    // that name and Emma wins" is exactly that question — it has been true of
    // `/exit` since the loop was written and was written down nowhere.
    for name in shadowed(&project) {
        writeln!(
            out,
            "               ! /{name} is shadowed by Emma's own /{name} and cannot be reached. \
             Rename the file."
        )?;
    }

    let hooks = snapshot["hooks"].as_array().cloned().unwrap_or_default();
    writeln!(
        out,
        "hooks          {}",
        if hooks.is_empty() {
            "(none)".to_string()
        } else {
            format!("{} configured", hooks.len())
        }
    )?;
    for hook in &hooks {
        writeln!(
            out,
            "               {} {} {}",
            hook["event"].as_str().unwrap_or("?"),
            hook["name"].as_str().unwrap_or("?"),
            hook["hash"].as_str().unwrap_or("?")
        )?;
    }

    let home = auth::home_dir();

    // The permission rules, in the order they are consulted, each with the file
    // it came from. This is the answer to "why did it not ask me about that" and
    // to "why is it still asking" — and it is the visibility the whole persisted
    // grant rests on. A permission the user cannot see is a permission they have
    // forgotten they granted; this command is where they see it.
    let mut entries = harness.permissions().to_vec();
    entries.extend(emma_harness::user_permissions(home.as_deref())?);
    writeln!(
        out,
        "permissions    {}",
        if entries.is_empty() {
            "(none — every write, command and host is asked about)".to_string()
        } else {
            format!("{} rule(s)", entries.len())
        }
    )?;
    for entry in &entries {
        writeln!(
            out,
            "               {:<5} {}   {}",
            entry.kind.word(),
            entry.rule,
            entry.source.display()
        )?;
    }
    // The rules that will not do anything, said again here even though startup
    // says it too: this is the command somebody runs *because* a rule did not
    // fire, and making them re-read scrollback for the reason is a poor answer.
    for note in crate::permissions::Rules::parse(&entries).1 {
        writeln!(out, "               ! {note}")?;
    }
    writeln!(
        out,
        "               a remembered grant is written to {}",
        crate::permissions::file_for(&harness.root).display()
    )?;

    for line in configured(home.as_deref(), &|name| std::env::var(name).ok(), live) {
        writeln!(out, "{line}")?;
    }
    writeln!(out, "\nno model was called.")?;
    Ok(())
}

/// Project commands a built-in makes unreachable.
///
/// **The built-in wins, and that is already the de facto rule**: `main` has
/// always matched `/exit` before `Harness::expand_command`, so
/// `.claude/commands/exit.md` has never been reachable. The alternative — a
/// directory being able to shadow `/model` or `/clear` — means a checkout can
/// take away Emma's own control surface, which is a small supply-chain hole for
/// no benefit. What changes here is only that it is said out loud, in the two
/// places somebody would look: this report and the `/` menu.
pub fn shadowed(project: &[&str]) -> Vec<String> {
    project
        .iter()
        .filter(|name| {
            crate::session_command::BUILTINS
                .iter()
                .any(|(builtin, _)| builtin.eq_ignore_ascii_case(name))
        })
        .map(|name| (*name).to_string())
        .collect()
}

/// The provider, the model and where the key came from — the three lines
/// somebody debugging "why is it using that" actually needs.
///
/// A function returning lines rather than four `println!`s, because this is the
/// half of `config check` that has a wrong answer worth pinning: a provider
/// nobody can run, a model that came from somewhere other than where the user
/// thinks, or a stored key silently shadowed by an environment variable.
///
/// The environment arrives as a lookup rather than being read here, for the
/// same reason `auth::resolve` takes its value: a test can pin either source
/// without mutating process state every other test in the binary shares.
fn configured(
    home: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
    live: Option<Live>,
) -> Vec<String> {
    let (kind, resolved) = match settings::resolve_kind(None, None, home) {
        Ok(both) => both,
        // Loud, and not fatal: `config check` is the command people run when
        // something is wrong, so the one thing it must never do is fail to
        // report the thing that is wrong.
        Err(e) => {
            return vec![
                format!("provider       NOT USABLE — {e}"),
                "               nothing will run until `emma set-provider <name>` names one \
                 this build supports"
                    .into(),
            ]
        }
    };
    // What is on disk, and — when there is a running session — what is actually
    // in force. The two differ the moment somebody runs `/model`, and the disk
    // value is then wrong about the one thing they just changed.
    let mut lines = match &live {
        Some(live) if live.model != resolved.model => vec![
            format!(
                "provider       {}  ({})",
                resolved.provider, resolved.provider_source
            ),
            format!("model          {}  (this session, /model)", live.model),
            format!(
                "               settings.json still says {} — /model {} --save writes it",
                resolved.model, live.model
            ),
        ],
        _ => vec![
            format!(
                "provider       {}  ({})",
                resolved.provider, resolved.provider_source
            ),
            format!(
                "model          {}  ({})",
                resolved.model, resolved.model_source
            ),
        ],
    };

    // Reported, never printed. Whether a key resolves is the question; which
    // key it is, is not.
    //
    // The two sources are asked in the same order `auth::load_default` asks
    // them, and separately rather than through one call, because there is no
    // home to hand `resolve` when the platform will not say where home is — and
    // substituting a relative path there would make this the one place in the
    // program that looks for a key in the working directory.
    let env = env(kind.env_var()).filter(|k| !k.trim().is_empty());
    match (env, home) {
        (Some(_), _) => {
            lines.push(format!("api key        found ({})", kind.env_var()));
            if home.is_some_and(|h| auth::stored_providers(h).iter().any(|p| p == kind.name())) {
                // The hour this saves: a key stored, a key exported, and no way
                // to tell which one the 401 came from.
                lines.push(format!(
                    "               the environment overrides the key stored for {}",
                    kind.name()
                ));
            }
        }
        (None, Some(home)) => match auth::resolve(kind, None, home) {
            Ok(_) => lines.push("api key        found (stored file)".into()),
            Err(e) => lines.push(format!("api key        NOT FOUND — {e}")),
        },
        (None, None) => lines.push(format!(
            "api key        NOT FOUND — {}",
            emma_llm::AuthError::NoHome
        )),
    }

    // Enough to answer "did I store that key?", and not enough to be a
    // disclosure: names of providers, never a byte of any key.
    let others: Vec<String> = home
        .map(auth::stored_providers)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p != kind.name())
        .collect();
    if !others.is_empty() {
        lines.push(format!(
            "               a key is also stored for: {}",
            others.join(", ")
        ));
    }
    lines
}

// `rename_auth` used to live here: a `replace` that rewrote `emma auth` to
// `emma api` at every boundary where a provider error was printed, because
// `emma-llm` composed the sentence and named a command that does not exist.
// The sentence itself was fixed in `emma-llm`, so the rewrite matched nothing —
// and a patch that no longer patches anything is worse than none, because the
// next person to read it learns a rule about the message that is not true. The
// fix belongs in the crate that owns the string, which is where it now is.

fn or_none(s: &str) -> String {
    if s.is_empty() {
        "(none)".into()
    } else {
        s.to_string()
    }
}

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Every one of these drives the private half of a command — the one that takes
// a home directory and a writer rather than finding them. The public halves are
// two lines each and prompt for a secret; what is worth pinning is what lands
// on disk, and that is all in here.
//
// Two of them guard properties that would be silent if they broke: the owner's
// pre-provider key and model surviving an upgrade, and a provider this build
// cannot run never quietly becoming Anthropic.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn anthropic() -> &'static dyn emma_llm::ProviderKind {
        emma_llm::kind("anthropic").unwrap()
    }

    fn say(f: impl FnOnce(&mut dyn Write) -> Result<()>) -> String {
        let mut out: Vec<u8> = Vec::new();
        f(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn set_provider_writes_the_key_first_and_then_selects_it() {
        let home = tempfile::tempdir().unwrap();
        let said = say(|out| {
            store_provider(
                home.path(),
                anthropic(),
                "sk-ant-new",
                Some("claude-sonnet-4-5"),
                out,
            )
        });
        assert_eq!(
            auth::resolve(anthropic(), None, home.path())
                .unwrap()
                .expose(),
            "sk-ant-new"
        );
        let (_, resolved) = settings::resolve_kind(None, None, Some(home.path())).unwrap();
        assert_eq!(resolved.provider, "anthropic");
        assert_eq!(resolved.model, "claude-sonnet-4-5");
        assert!(said.contains("credentials.json"), "{said}");
        assert!(said.contains("settings.json"), "{said}");
        assert!(said.contains("claude-sonnet-4-5"), "{said}");
        // Stored, and never repeated back.
        assert!(!said.contains("sk-ant-new"), "the key was echoed: {said}");
    }

    #[test]
    fn a_provider_stored_without_a_model_says_which_command_finishes_the_job() {
        // The resting place: a key is a reasonable thing to have on its own, and
        // somebody who stops here must not have to guess what is left.
        let home = tempfile::tempdir().unwrap();
        let said = say(|out| store_provider(home.path(), anthropic(), "sk-ant-x", None, out));
        assert!(said.contains("emma set-model"), "{said}");
        assert!(said.contains(emma_llm::DEFAULT_MODEL), "{said}");
    }

    #[test]
    fn the_setup_that_exists_today_survives_the_upgrade() {
        // The owner's live `~/.emma`, reproduced: a flat `api_key`, a Brave key
        // beside it that belongs to the web tools, and a bare `{"model": …}`.
        // If this goes red, an upgrade quietly took away somebody's working
        // configuration — the failure that arrives with no error message.
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".emma")).unwrap();
        std::fs::write(
            emma_llm::auth::credentials_path(home.path()),
            r#"{"api_key":"sk-ant-live","brave_search_api_key":"BSA-live"}"#,
        )
        .unwrap();
        std::fs::write(
            settings::path(home.path()),
            r#"{"model":"claude-sonnet-5"}"#,
        )
        .unwrap();

        // Before touching anything: the key and the model are the ones he set.
        let lines = configured(Some(home.path()), &|_| None, None).join("\n");
        assert!(lines.contains("anthropic"), "{lines}");
        assert!(lines.contains("claude-sonnet-5"), "{lines}");
        assert!(
            lines.contains("api key        found (stored file)"),
            "{lines}"
        );

        // …and after both kinds of write, still — including the key this module
        // does not own, which a whole-file rewrite would have deleted.
        say(|out| set_model_at(home.path(), "claude-opus-5", None, out));
        say(|out| store_provider(home.path(), anthropic(), "sk-ant-live", None, out));
        assert_eq!(
            auth::resolve(anthropic(), None, home.path())
                .unwrap()
                .expose(),
            "sk-ant-live"
        );
        let raw = std::fs::read_to_string(emma_llm::auth::credentials_path(home.path())).unwrap();
        assert!(raw.contains("BSA-live"), "{raw}");
        let (_, resolved) = settings::resolve_kind(None, None, Some(home.path())).unwrap();
        assert_eq!(resolved.model, "claude-opus-5");
        assert_eq!(resolved.provider, "anthropic");
    }

    #[test]
    fn set_model_needs_a_provider_and_names_the_command_that_sets_one() {
        let home = tempfile::tempdir().unwrap();
        let err = set_model_at(home.path(), "claude-x", None, &mut Vec::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("emma set-provider"), "{err}");
        // Nothing was written: a model with no provider is not a state to be in.
        assert!(!settings::path(home.path()).exists());
    }

    #[test]
    fn a_model_is_never_stored_for_a_provider_this_build_cannot_run() {
        // The silent failure this prevents: `emma set-model gpt-5.5 --provider
        // openai` writing a setting that Anthropic then quietly answers.
        let home = tempfile::tempdir().unwrap();
        let err = set_model_at(home.path(), "gpt-5.5", Some("openai"), &mut Vec::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("openai"), "{err}");
        assert!(err.contains("anthropic"), "{err}");
        assert!(!settings::path(home.path()).exists());
        // The same refusal from the other door, and before a key is asked for:
        // this call supplies one and it must never be stored anywhere.
        let err = set_provider("openai", Some("sk-should-never-be-read".into()), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("openai"), "{err}");
    }

    #[test]
    fn set_model_says_when_there_is_no_key_for_the_provider_yet() {
        let home = tempfile::tempdir().unwrap();
        let said = say(|out| set_model_at(home.path(), "claude-x", Some("anthropic"), out));
        assert!(said.contains("no API key"), "{said}");
        assert!(said.contains("emma set-provider anthropic"), "{said}");
        // Named a provider without switching to it: `--provider` sets that
        // provider's remembered model and leaves the selection alone.
        assert!(settings::load(home.path()).provider.is_none());
        assert_eq!(settings::load(home.path()).models["anthropic"], "claude-x");
    }

    #[test]
    fn config_check_says_where_the_key_came_from() {
        let home = tempfile::tempdir().unwrap();
        store_provider(
            home.path(),
            anthropic(),
            "sk-ant-stored",
            Some("claude-x"),
            &mut Vec::new(),
        )
        .unwrap();
        emma_llm::auth::store(
            home.path(),
            "elsewhere",
            &emma_llm::ApiKey::new("other-key"),
        )
        .unwrap();

        let lines = configured(Some(home.path()), &|_| None, None).join("\n");
        assert!(
            lines.contains("provider       anthropic  (settings.json)"),
            "{lines}"
        );
        assert!(
            lines.contains("model          claude-x  (settings.json)"),
            "{lines}"
        );
        assert!(lines.contains("found (stored file)"), "{lines}");
        assert!(lines.contains("also stored for: elsewhere"), "{lines}");
        assert!(!lines.contains("other-key"), "a key was printed: {lines}");

        // The hour this line saves: an exported variable silently outranking
        // the key that was just stored.
        let lines = configured(
            Some(home.path()),
            &|name| (name == "ANTHROPIC_API_KEY").then(|| "sk-ant-env".to_string()),
            None,
        )
        .join("\n");
        assert!(lines.contains("found (ANTHROPIC_API_KEY)"), "{lines}");
        assert!(lines.contains("the environment overrides"), "{lines}");
    }

    #[test]
    fn config_check_reports_an_unusable_provider_rather_than_hiding_it() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".emma")).unwrap();
        std::fs::write(settings::path(home.path()), r#"{"provider":"openai"}"#).unwrap();
        let lines = configured(Some(home.path()), &|_| None, None).join("\n");
        assert!(lines.contains("NOT USABLE"), "{lines}");
        assert!(lines.contains("openai"), "{lines}");
        // Nothing may claim a model or a key under a provider that cannot run.
        assert!(!lines.contains("api key        found"), "{lines}");
    }
}

// endregion: Tests
