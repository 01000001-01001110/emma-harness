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

- Use the tools. Do not describe what you would do — read the file, make the
  edit, run the command.
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
    let config = root.join("config.json");
    let rules = persona.join("rules.md");
    std::fs::write(&config, CONFIG).with_context(|| format!("writing {}", config.display()))?;
    std::fs::write(&rules, RULES).with_context(|| format!("writing {}", rules.display()))?;

    writeln!(out, "created {}", config.display())?;
    writeln!(out, "created {}", rules.display())?;
    writeln!(
        out,
        "\nEdit {} to say what this agent should know and how it should work.\n\n\
         Next:\n  \
         emma config check      load it and print exactly what the model will be told\n  \
         emma \"<goal>\"          state a goal\n\n\
         If `emma` says it cannot find an API key, store one with `emma api`.",
        rules.display()
    )?;
    Ok(())
}

// endregion: init

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
