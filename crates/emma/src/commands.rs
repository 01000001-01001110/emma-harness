//! `emma api`, `emma model`, `emma init`, `emma config check` — the four things
//! that run without a model call, and therefore the four things worth reaching
//! for when something is wrong.
//!
//! `api` and `model` write under `~/.emma/`; `config check` writes nothing at
//! all. Neither of the first two touches the project directory — Emma runs
//! inside repositories, and a file it creates next to someone's code is a file
//! their next `git add .` publishes.
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

/// Store an API key under the user's home directory.
///
/// The key may be given as an argument for provisioning scripts, or typed at a
/// prompt that does not echo it. Echoing would put it on the screen, in a
/// screen recording, and — if it was pasted after the command — in shell
/// history. `rpassword` exists in the dependency list for this one call.
pub fn api(given: Option<String>) -> Result<()> {
    let home = auth::home_dir().context(
        "the home directory could not be determined, so there is nowhere to store a key",
    )?;
    let key = match given {
        Some(key) => key,
        None if std::io::stdin().is_terminal() => {
            rpassword::prompt_password("Anthropic API key (not echoed): ")
                .context("reading the key")?
        }
        // Piped in: `printf %s "$KEY" | emma api`.
        None => {
            let mut raw = String::new();
            std::io::stdin()
                .read_line(&mut raw)
                .context("reading the key from stdin")?;
            raw
        }
    };
    let key = key.trim();
    if key.is_empty() {
        bail!("no key was given; nothing was written");
    }
    let path = auth::store(&home, &ApiKey::new(key))?;
    println!("stored {}", path.display());
    if std::env::var_os(auth::ENV_VAR).is_some() {
        // Otherwise the next run uses the environment and the user concludes
        // the file did not work.
        eprintln!(
            "note: {} is set in this shell and takes precedence over the stored key",
            auth::ENV_VAR
        );
    }
    Ok(())
}

/// `emma model` reports; `emma model <name>` sets.
pub fn model(name: Option<String>) -> Result<()> {
    let home = auth::home_dir().context(
        "the home directory could not be determined, so there is nowhere to store a preference",
    )?;
    match name {
        Some(name) => {
            let mut current = settings::load(&home);
            current.model = Some(name.clone());
            let path = settings::save(&home, &current)?;
            println!("model set to {name}");
            println!("stored {}", path.display());
        }
        None => {
            let (model, source) = settings::resolve(None, Some(&home));
            println!("{model}  ({source})");
        }
    }
    Ok(())
}

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

/// Load the harness, apply the tool allowlist, and report — without calling a
/// model, which is the whole point. Everything that can fail at startup fails
/// here, where the message is the only output rather than a preamble to one.
pub fn config_check(
    harness: &Harness,
    tools: &Registry,
    cwd: &Path,
    unavailable: &[String],
) -> Result<()> {
    let snapshot = harness.snapshot();
    println!("cwd            {}", cwd.display());
    println!(
        "harness        {}",
        snapshot["root"].as_str().unwrap_or("?")
    );
    println!(
        "flavor         {}",
        snapshot["flavor"].as_str().unwrap_or("?")
    );
    println!(
        "persona        {}",
        snapshot["persona"].as_str().unwrap_or("(none)")
    );
    println!(
        "instructions   {} ({} bytes{})",
        harness.instructions_hash(),
        harness.instructions.len(),
        if harness.is_empty() {
            ", empty harness"
        } else {
            ""
        }
    );
    println!("config         {}", harness.config_hash);
    println!("tools          {}", tools.names().join(", "));
    // The tools that were built and then left out because this machine cannot
    // run them. This is the one command whose job is to answer "why can it not
    // do X", and a capability absent for a fixable reason — no browser, no
    // search key — is exactly the question it is asked.
    for line in unavailable {
        println!("               {line}");
    }
    println!("tool schema    {}", tools.schema_hash());
    println!(
        "skills         {}",
        or_none(&harness.skill_names().join(", "))
    );
    // The delegation catalogue, and — separately — the files that were found and
    // not offered. A catalogue quietly shorter than the directory is the gap
    // nobody notices until the model cannot find an agent that is plainly there,
    // and this is the command whose job is to answer "why can it not do X".
    let types = harness.agent_types();
    println!(
        "agents         {}",
        or_none(
            &types
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    );
    // Said explicitly, because the tools line above does not carry it and a
    // reader would reasonably conclude delegation is off. `Delegate` is built
    // from a provider, and this command deliberately runs without one so that it
    // still answers when the key is the thing that is wrong.
    if !types.is_empty() {
        println!(
            "               offered to the model as the `Delegate` tool, which is registered \
             at run time and so is not in the tools line above"
        );
    }
    for note in harness.agent_notes() {
        println!("               {note}");
    }
    println!(
        "commands       {}",
        or_none(
            &harness
                .command_names()
                .iter()
                .map(|c| format!("/{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    );

    let hooks = snapshot["hooks"].as_array().cloned().unwrap_or_default();
    println!(
        "hooks          {}",
        if hooks.is_empty() {
            "(none)".to_string()
        } else {
            format!("{} configured", hooks.len())
        }
    );
    for hook in &hooks {
        println!(
            "               {} {} {}",
            hook["event"].as_str().unwrap_or("?"),
            hook["name"].as_str().unwrap_or("?"),
            hook["hash"].as_str().unwrap_or("?")
        );
    }

    let home = auth::home_dir();
    let (model, source) = settings::resolve(None, home.as_deref());
    println!("model          {model}  ({source})");

    // Reported, never printed. Whether a key resolves is the question; which
    // key it is, is not.
    match auth::load_default() {
        Ok(_) if std::env::var_os(auth::ENV_VAR).is_some() => {
            println!("api key        found ({})", auth::ENV_VAR)
        }
        Ok(_) => println!("api key        found (stored file)"),
        Err(e) => println!("api key        NOT FOUND — {e}"),
    }
    println!("\nno model was called.");
    Ok(())
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
