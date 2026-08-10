//! `emma api`, `emma model`, `emma config check` — the three things that run
//! without a model call, and therefore the three things worth reaching for when
//! something is wrong.
//!
//! `api` and `model` write under `~/.emma/`; `config check` writes nothing at
//! all. None of the three touches the project directory — Emma runs inside
//! repositories, and a file it creates next to someone's code is a file their
//! next `git add .` publishes.

use std::io::IsTerminal;
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
    let home = auth::home_dir()
        .context("the home directory could not be determined, so there is nowhere to store a preference")?;
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

/// Load the harness, apply the tool allowlist, and report — without calling a
/// model, which is the whole point. Everything that can fail at startup fails
/// here, where the message is the only output rather than a preamble to one.
pub fn config_check(harness: &Harness, tools: &Registry, cwd: &Path) -> Result<()> {
    let snapshot = harness.snapshot();
    println!("cwd            {}", cwd.display());
    println!("harness        {}", snapshot["root"].as_str().unwrap_or("?"));
    println!("flavor         {}", snapshot["flavor"].as_str().unwrap_or("?"));
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
    println!("tool schema    {}", tools.schema_hash());
    println!("skills         {}", or_none(&harness.skill_names().join(", ")));
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
        Err(e) => println!("api key        NOT FOUND — {}", rename_auth(&e.to_string())),
    }
    println!("\nno model was called.");
    Ok(())
}

/// `emma-llm` composes its own "no API key" sentence and names the command that
/// stores one. It says `emma auth`; the command is `emma api`. Both crates
/// cannot own that string, and pointing a stuck user at a command that does not
/// exist is the worst of the three options — so it is corrected here, at the
/// one boundary where the message is printed, until the sentence itself moves
/// or changes. A `replace` rather than a rewrite so that everything else the
/// provider said survives intact.
pub fn rename_auth(message: &str) -> String {
    message.replace("emma auth", "emma api")
}

fn or_none(s: &str) -> String {
    if s.is_empty() {
        "(none)".into()
    } else {
        s.to_string()
    }
}
