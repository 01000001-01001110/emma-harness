//! Emma's own commands, typed at the goal prompt or picked from the `/` menu.
//!
//! # Why the parsing happens here and not in `term/`
//!
//! `MenuKey::Accept` turns the highlighted row into the string `/{name}` and
//! sends it down the *same* `mpsc` channel a typed goal goes down, so a menu
//! pick and a typed line are indistinguishable by the time `main` sees them.
//! That is the property this module is built on rather than around:
//!
//! - The menu is a **completion affordance, not a dispatch mechanism.** One code
//!   path means one place a command can be wrong.
//! - The fallback path — a pipe, `EMMA_NO_FRAME`, a 60×6 window — has **no menu
//!   at all**. A command dispatched from the menu would not exist there, and
//!   `term.rs` states the rule this project runs on: *the fallback is the
//!   product*.
//! - `term/menu.rs`'s own doc records that nothing in it draws, locks or reads a
//!   terminal. A command *executes*; putting execution behind the menu would
//!   cost that file its testability.
//!
//! So this file is pure — a string in, a [`SessionCommand`] out — and [`run`] is
//! the one place any of them does anything.
//!
//! # The rule a later contributor will be tempted to break
//!
//! **No session command opens its own reader.** Every in-session question goes
//! through `Approvals::read_line`, which drains first, and is drawn through the
//! `Term` prompt calls, which suppress the menu. A command that reads stdin
//! directly re-creates the two-readers race that `approval.rs` and
//! `term/input.rs` both exist to prevent, and the symptom is a stale `y`
//! selecting a model. Nothing in the current set asks a question — `/clear` is
//! deliberately unconfirmed — and that is the cheapest way to keep the rule.
//!
//! # There is no "mid-goal"
//!
//! `main`'s loop awaits `run_goal` to completion and nothing polls stdin while
//! it runs, so every one of these executes *between* goals. That is not a
//! limitation this feature invented; it is the only moment input is read.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use emma_harness::Harness;
use emma_llm::auth::ApiKey;
use emma_llm::{Provider, ProviderKind};
use emma_tool_api::Registry;

use crate::agent::{Agent, Compacted};
use crate::approval::Approvals;
use crate::term::Term;

// region: The vocabulary
// ---------------------------------------------------------------------------
// The vocabulary
//
// One list. `term/menu.rs` builds its rows from it and `cli::SESSION_HELP`
// documents it, both under a test — so a command cannot exist without a menu
// row, and a menu row cannot exist without a paragraph saying what it does.
// ---------------------------------------------------------------------------

/// `(name, one line)`, in the order the menu shows them.
///
/// Ordered by what somebody reaches for, not alphabetically: the owner's
/// complaint was that the menu offered only the two ways to leave, so the two
/// ways to leave are last.
pub const BUILTINS: &[(&str, &str)] = &[
    ("help", "what this session understands"),
    (
        "model",
        "the model in force — /model <id> changes it for this session",
    ),
    (
        "compact",
        "summarise the finished goals now, rather than at the limit",
    ),
    (
        "clear",
        "start a fresh conversation — this session's grants are kept",
    ),
    (
        "copy",
        "put the last answer on the clipboard — the source, not the screen",
    ),
    (
        "theme",
        "the colours — /theme <name> selects one, from the next start",
    ),
    (
        "config",
        "what this run resolved: harness, tools, rules, key",
    ),
    ("agents", "what each subagent type has cost and produced"),
    ("resume", "how to continue an earlier session"),
    ("exit", "end this session"),
    ("quit", "end this session — the same thing as /exit"),
    // **After `exit`, deliberately.** The menu lists in this order, so a prefix
    // that matches both highlights whichever comes first — and `/ex` followed by
    // Enter has meant "end this session" for the life of this command set.
    // Putting `export` earlier would silently repurpose that muscle memory, and
    // the two outcomes are not close: one ends the session and one writes a
    // file. A new command does not get to take an established prefix.
    (
        "export",
        "write this conversation to a file — works where /copy cannot",
    ),
];

/// One of Emma's own commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCommand {
    /// `/exit`, `/quit`.
    Exit,
    Help,
    Config,
    Agents,
    /// `/model`, `/model <id>`, `/model <id> --save`, `/model --save`.
    ///
    /// `id` is whatever single word followed the command, unvalidated: this
    /// module is pure and a model id is checked where it can be refused with
    /// the state left alone. See [`refuse_a_bad_id`].
    Model {
        id: Option<String>,
        save: bool,
    },
    /// `/resume`, `/resume <id>` — advice, not an action. See [`run`].
    Resume(Option<String>),
    /// `/compact`, `/compact all`, and `/compact <words>` — which is refused
    /// out loud rather than silently ignoring the words.
    Compact {
        everything: bool,
        instruction: Option<String>,
    },
    Clear,
    /// `/copy` — put the last answer on the clipboard.
    ///
    /// **The source, not the screen.** The text comes from the session log,
    /// which holds what the model actually wrote; the transcript on screen has
    /// been wrapped to a column and interleaved with the sidebar, so a terminal
    /// selection of the same answer arrives with borders and gutters in every
    /// line. That is the owner's actual complaint — the mess, not the modifier —
    /// and copying the source sidesteps it entirely rather than competing with
    /// the terminal for the mouse.
    Copy,
    /// `/export`, `/export <path>` — write the conversation to a file.
    ///
    /// The answer for every path `/copy` refuses. A file needs no escape byte,
    /// so this works on `-p`, on a pipe and with no console, which is where a
    /// clipboard write is forbidden and where somebody most wants the text.
    Export(Option<String>),
    /// `/theme`, `/theme <name>`.
    ///
    /// There is no `--save`, and that is not an omission: a theme is read once
    /// at startup, so selecting one *is* writing it down. See the `/theme`
    /// region for the whole of that argument.
    Theme {
        name: Option<String>,
    },
    /// A built-in name with arguments it does not take. Carried rather than
    /// dropped so the answer is a usage line instead of a model call.
    Misuse {
        name: &'static str,
        usage: &'static str,
    },
}

/// Recognise one of Emma's own commands. `None` is "not one of ours" — a
/// project command, a path, or ordinary text.
///
/// **A bare word is never a command.** `model` with no slash is
/// `cli::typed_at_the_prompt`'s business, and `initialise the auth module` is
/// somebody's actual work. The leading `/` is the whole of the disambiguation,
/// which is why it is also what the menu keys on.
///
/// **A near-miss is not a hit.** `/models` and `/clearance` are not commands
/// here; they fall through to `Harness::expand_command` and then to the model,
/// exactly as any other `/word` does.
pub fn parse(line: &str) -> Option<SessionCommand> {
    let rest = line.trim().strip_prefix('/')?;
    let mut words = rest.split_whitespace();
    let name = words.next()?.to_ascii_lowercase();
    // `/usr/bin/env` and `/api/v2 is returning 500` are a path and a sentence.
    // Only the *name* is checked: an argument may well contain a slash, and
    // `/model src/lib.rs` has to reach the command in order to be refused by it
    // — falling through would send it to the model as a goal, which is the
    // expensive mistake `cli.rs` has already paid for once.
    if name.contains('/') {
        return None;
    }
    let args: Vec<&str> = words.collect();
    // The command is spelled in full or it is not this command. Prefix matching
    // belongs in the menu, where a wrong guess costs a keystroke; here it would
    // cost a conversation.
    match name.as_str() {
        "exit" | "quit" => Some(SessionCommand::Exit),
        "help" => Some(SessionCommand::Help),
        "config" => Some(SessionCommand::Config),
        "agents" => Some(SessionCommand::Agents),
        "clear" => Some(SessionCommand::Clear),
        "copy" => Some(SessionCommand::Copy),
        "export" => Some(match args.as_slice() {
            [] => SessionCommand::Export(None),
            [one] if !one.starts_with('-') => SessionCommand::Export(Some((*one).to_string())),
            // A flag here is somebody expecting options this does not have, and
            // guessing which of several they meant is how a file lands somewhere
            // nobody asked for.
            _ => SessionCommand::Misuse {
                name: "export",
                usage: EXPORT_USAGE,
            },
        }),
        "theme" => Some(match args.as_slice() {
            [] => SessionCommand::Theme { name: None },
            // A flag is never a theme name, and `--save` is the one somebody
            // will type here out of `/model` habit. It gets the usage line,
            // which says why the flag does not exist — a flag that is silently
            // accepted and does nothing is worse than one that is not there.
            [one] if !one.starts_with('-') => SessionCommand::Theme {
                name: Some((*one).to_string()),
            },
            _ => SessionCommand::Misuse {
                name: "theme",
                usage: THEME_USAGE,
            },
        }),
        "resume" => Some(SessionCommand::Resume(args.first().map(|s| s.to_string()))),
        "compact" => Some(match args.as_slice() {
            [] => SessionCommand::Compact {
                everything: false,
                instruction: None,
            },
            [one] if one.eq_ignore_ascii_case("all") => SessionCommand::Compact {
                everything: true,
                instruction: None,
            },
            words => SessionCommand::Compact {
                everything: false,
                instruction: Some(words.join(" ")),
            },
        }),
        "model" => {
            let save = args.contains(&"--save");
            let rest: Vec<&str> = args.iter().copied().filter(|a| *a != "--save").collect();
            match rest.as_slice() {
                [] => Some(SessionCommand::Model { id: None, save }),
                [id] => Some(SessionCommand::Model {
                    id: Some((*id).to_string()),
                    save,
                }),
                // Two words is not a model id, and guessing which one was meant
                // is how `emma set-provider anthropic please` stored a key for a
                // provider with a space in its name. Same ruling, same reason.
                _ => Some(SessionCommand::Misuse {
                    name: "model",
                    usage: MODEL_USAGE,
                }),
            }
        }
        _ => None,
    }
}

const MODEL_USAGE: &str = "/model                     what is running, and what it accepts\n\
                           /model <id>                use <id> for the rest of this session\n\
                           /model <id> --save         …and remember it for the next one\n\
                           /model --save              remember what is already running";

const THEME_USAGE: &str =
    "/theme                     what is available, and which one is selected\n\
     /theme <name>              select it. There is no --save: a theme is read once, at\n\
     \x20                          startup, so writing the name to settings.json is the\n\
     \x20                          whole of it — and it is the next start that shows it.";

// endregion: The vocabulary

// region: What a command can reach
// ---------------------------------------------------------------------------
// What a command can reach
//
// One struct rather than eleven arguments, and it is deliberately the *whole*
// of the session: a command that needed something not in here would be a
// command reaching around the loop.
// ---------------------------------------------------------------------------

/// Everything the commands are allowed to touch.
pub struct Session<'a, 'agent> {
    pub agent: &'a mut Agent<'agent>,
    pub term: &'a Term,
    pub approvals: &'a Approvals,
    pub harness: &'a Harness,
    pub tools: &'a Registry,
    pub cwd: &'a Path,
    pub session_dir: Option<&'a Path>,
    /// The tools `web_tools` built and left out, for `/config` to repeat. Same
    /// list the CLI path prints.
    pub unavailable: &'a [String],
    /// The provider in force. A `&mut` to `main`'s own binding rather than a
    /// copy, so `/model` cannot leave the two disagreeing about what is running.
    pub provider: &'a mut Arc<dyn Provider>,
    /// The same fact as `provider`, in the cell `Delegate` reads when a
    /// delegation actually starts. Written here and nowhere else — see
    /// [`crate::agent::Running`].
    pub running: &'a crate::agent::Running,
    pub kind: &'static dyn ProviderKind,
    pub key: ApiKey,
    /// Where the transcript is, for the status row `/model` has to re-set.
    pub log_path: PathBuf,
    pub home: Option<PathBuf>,
    /// The theme name `settings.json` held when this process started, which is
    /// therefore the one on screen for the life of it.
    ///
    /// Snapshotted rather than re-read, because `/theme` writes that same key:
    /// after one selection the file says one thing and the screen shows
    /// another, and this is the only copy of the second fact. `None` is the
    /// built-in.
    pub theme_at_start: Option<String>,
}

/// Whether the loop keeps going. Two variants rather than plumbing a `break`
/// out of a function.
#[derive(Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Exit,
}

/// Run one command. Every path here either prints or changes this process, and
/// none of them reaches a model or the network.
pub async fn run(cmd: SessionCommand, s: &mut Session<'_, '_>) -> Flow {
    match cmd {
        SessionCommand::Exit => return Flow::Exit,
        SessionCommand::Help => say(s.term, crate::cli::SESSION_HELP),
        SessionCommand::Misuse { name, usage } => say(s.term, &format!("/{name} takes:\n{usage}")),
        SessionCommand::Agents => {
            match capture(|out| crate::commands::agents(s.session_dir, out)) {
                Ok(text) => say(s.term, &text),
                Err(e) => s.term.warn(&format!("/agents: {e}")),
            }
        }
        SessionCommand::Copy => copy_last_answer(s),
        SessionCommand::Export(path) => export_conversation(s, path.as_deref()),
        SessionCommand::Config => {
            let live = crate::commands::Live {
                provider: s.kind.name().to_string(),
                model: s.provider.model_id().to_string(),
            };
            match capture(|out| {
                crate::commands::config_check(
                    s.harness,
                    s.tools,
                    s.cwd,
                    s.unavailable,
                    Some(live.clone()),
                    out,
                )
            }) {
                Ok(text) => say(s.term, &text),
                Err(e) => s.term.warn(&format!("/config: {e}")),
            }
        }
        SessionCommand::Resume(_) => say(s.term, RESUME_ADVICE),
        SessionCommand::Compact {
            everything,
            instruction,
        } => compact(s, everything, instruction),
        SessionCommand::Clear => clear(s).await,
        SessionCommand::Model { id, save } => model(s, id, save),
        SessionCommand::Theme { name } => theme(s, name),
    }
    Flow::Continue
}

/// `/resume`, which does not belong in a running session and says so.
///
/// Four concrete obstacles, and none of them cosmetic: `Agent::resuming` is
/// consumed by the first goal and has no defined interleaving with the goals
/// already in this conversation; `SessionLog` is open on *this* session's path
/// and the status row names it; bare `--resume` picks the newest session
/// started in this directory, which inside a session is the one you are in; and
/// the continuity warnings are computed at startup against hashes that `/model`
/// can now change underneath them.
///
/// So it answers with the thing that does work, in the shape
/// `cli::typed_at_the_prompt` already established for a command that runs
/// somewhere else.
const RESUME_ADVICE: &str = "\
/resume continues a session that was interrupted, and it has to happen before one
starts: this process already has a transcript open and a conversation in it.

Use /exit, then `emma --resume` in your shell — or `emma --resume sess-…` for a
particular one. Bare `emma --resume` continues the newest session started in
this directory. /config prints this session's transcript path.";

// endregion: What a command can reach

// region: /model
// ---------------------------------------------------------------------------
// /model
//
// The one the owner asked for: changing model meant quitting the session.
//
// It reports rather than opening a picker, for the reason `commands.rs` gives
// about `emma model` — a habitual spelling must keep meaning what it meant —
// and because there is no picker to reuse yet. `notes/design/provider-and-model.md`
// §3.1's `select_model` does not exist in this tree; writing a second selection
// implementation here is exactly what that note was written to prevent, so
// `/model` with no argument reports and the picker arrives with `select_model`.
// ---------------------------------------------------------------------------

fn model(s: &mut Session<'_, '_>, id: Option<String>, save: bool) {
    let running = s.provider.model_id().to_string();
    let Some(id) = id else {
        report_model(s, &running);
        if save {
            save_model(s, &running);
        }
        return;
    };
    if let Some(refusal) = refuse_a_bad_id(&id) {
        // Nothing has been touched at this point and nothing will be. A command
        // that half-applies — new provider, old status row, old `main` binding
        // — is worse than one that refuses, because the disagreement is
        // invisible until the bill arrives.
        s.term.warn(&refusal);
        return;
    }
    if id == running {
        s.term
            .note(&format!("{running} is already what this session is using."));
        if save {
            save_model(s, &running);
        }
        return;
    }

    let built = s.kind.build(s.key.clone(), Some(id.clone()));
    let was = s.agent.set_provider(built.clone());
    // All three in one place, so there is no window in which they disagree: the
    // agent's own client, `main`'s binding (which `/config` reports from), and
    // the cell a delegation resolves against.
    *s.provider = built.clone();
    s.running.set(built);
    // Otherwise the status row keeps naming the old model, which is the class
    // of untruth `term.rs` calls out about a meter measured against the wrong
    // cap.
    s.term.set_status(&id, s.cwd, &s.log_path);

    let mut lines = vec![
        format!("model     {id} for the rest of this session (was {was})"),
        format!("accepts   {}", accepts(&id)),
    ];
    if emma_llm::limits(&id) == emma_llm::models::UNKNOWN {
        // 8,192 output tokens is a visible degradation that would otherwise be
        // diagnosed as "Emma got worse". The id itself is not checked here —
        // the first call is the check.
        lines.push(format!(
            "note      this build has no recorded limits for {id}, so that is a conservative\n\
             \x20         floor rather than a fact about the model. Add a row to\n\
             \x20         crates/llm/src/models.rs to fix it. The id is not verified until the\n\
             \x20         first call."
        ));
    }
    // The number is deliberately introduced as a floor rather than as the
    // price. A cache entry is keyed by model as well as by bytes, so what is
    // re-read at full price is the whole cached *prefix* — the tool schemas and
    // the system prompt as well as the conversation — and the first live run of
    // this command measured 14,862 cache-weighted tokens on a conversation
    // worth fifteen. Quoting the conversation size as the cost would understate
    // it by three orders of magnitude on a fresh session.
    lines.push(format!(
        "cache     the cached prefix is gone — Anthropic keys a cache entry by model as well\n\
         \x20         as by bytes, so the next call re-reads it at full price. That prefix is\n\
         \x20         the tool schemas and the system prompt as well as the conversation, so\n\
         \x20         it costs more than the ~{} tokens of conversation below.",
        s.agent.estimated_context()
    ));
    lines.push(
        "history   the turns written by the previous model travel with the conversation. If\n\
         \x20         the next call is rejected for a thinking-block signature, Emma compacts\n\
         \x20         to summaries — which carry none — and retries once."
            .to_string(),
    );
    if save {
        save_model(s, &id);
    } else {
        lines.push(format!(
            "settings  unchanged. `/model {id} --save` remembers it for the next session."
        ));
    }
    say(s.term, &lines.join("\n"));
}

fn report_model(s: &Session<'_, '_>, running: &str) {
    let lines = [
        format!("model          {running}"),
        format!("provider       {}", s.kind.name()),
        format!("accepts        {}", accepts(running)),
        format!(
            "conversation   {} messages, roughly {} tokens (estimated)",
            s.agent.conversation().len(),
            s.agent.estimated_context()
        ),
        format!("change it      /model <id>{}", "  ·  /model <id> --save"),
    ];
    say(s.term, &lines.join("\n"));
}

/// What the chosen model will take, rendered from the one table that decides it.
///
/// This is the most valuable line the command can print and it is free:
/// `emma_llm::limits` is pure, public and already tested, and `agent.rs` asks
/// for 32,000 tokens at `xhigh` on every request regardless — the clamping is
/// silent, which is the problem this sentence fixes.
fn accepts(model: &str) -> String {
    let limits = emma_llm::limits(model);
    let effort = match limits.efforts.last() {
        Some(top) => format!("effort up to {}", top.as_str()),
        None => "no effort parameter (Emma's xhigh will not be sent)".to_string(),
    };
    format!(
        "max_tokens ≤ {} — Emma asks for 32000 — and {effort}",
        limits.max_tokens
    )
}

/// Why a string is not a model id, or `None`.
///
/// Deliberately a shape check rather than a membership check. `models.rs` has
/// no list to be a member of — its table is a snapshot of capabilities and an
/// unknown id there is *supposed* to fall to a conservative floor, so refusing
/// on it would turn every new model release into a bug in Emma. What is refused
/// is the class of argument that cannot be a model id at all: a path, a flag,
/// a sentence. Those would otherwise be accepted, applied, and discovered on
/// the next call as a provider error naming something the user did not type.
fn refuse_a_bad_id(id: &str) -> Option<String> {
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':');
    // A leading `-` is a flag somebody expected to be understood, not an id.
    // Accepting it would store `-p` as this session's model and report success.
    if id.is_empty() || id.starts_with('-') || !id.chars().all(ok) {
        return Some(format!(
            "`{id}` is not a model id, so nothing was changed. A model id is letters, digits, \
             `-`, `_`, `.` and `:` — for example claude-sonnet-4-5. `/model` on its own says \
             what is running."
        ));
    }
    None
}

fn save_model(s: &Session<'_, '_>, id: &str) {
    let Some(home) = &s.home else {
        s.term.warn(
            "--save needs a home directory to write settings.json to, and this platform did \
             not give one. The model is still in force for this session.",
        );
        return;
    };
    match crate::commands::write_model(home, s.kind.name(), id) {
        Ok(path) => s
            .term
            .note(&format!("settings  {id} written to {}", path.display())),
        Err(e) => s.term.warn(&format!("settings could not be written: {e}")),
    }
}

// endregion: /model

// region: /theme
// ---------------------------------------------------------------------------
// /theme
//
// **This command selects; it does not switch.** A theme is read once, at
// startup, and the owner ruled that this is enough — "we can set theme and
// restart the console to see it". So nothing here touches a `Palette`, a
// `Skin` or the frame. That is not a shortcut, it is what the ruling buys: a
// live swap has to reach every drawn surface at once, and the transcript holds
// already-styled rows drawn under the old colours, so the realistic outcome of
// repainting is a half-recoloured screen. There is no half here. There is a
// file, and a restart.
//
// What survives the cut, and is the whole of the risk that is left:
//
// - **The name is checked before it is written.** The loader is designed to
//   fall back with notices rather than refuse, so a bad name could not brick a
//   boot — but writing a selection already known to be broken is still writing
//   a lie into somebody's configuration.
// - **The write merges.** `~/.emma/settings.json` also holds the provider, the
//   per-provider models and the user-tool block, and a write path that replaced
//   a shared configuration document has already cost this project a stored
//   key. [`write_theme`] goes through the same load-mutate-save round trip
//   `commands::write_model` uses, for exactly that reason.
// - **`NO_COLOR` is untouched and unarguable.** The level is decided by
//   `Level::of` from the environment and a theme is not one of its inputs; at
//   `Level::None` the palette returns before a theme is consulted at all. So
//   `/theme` under `NO_COLOR` writes the preference and says plainly that the
//   screen will not change, which is the honest answer rather than a refusal —
//   the person may well be setting up for a terminal they will use later.
// ---------------------------------------------------------------------------

/// The compiled-in theme, and the one name a file may not claim.
///
/// Reserved so that a cloned repository cannot silently change what stock Emma
/// looks like by shipping `themes/emma.json` — the single door that
/// user-beats-project does not already close, since a project cannot select a
/// theme at all.
const BUILT_IN: &str = "emma";

/// One theme this run can see.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    name: String,
    /// `None` for the built-in, which is not a file.
    path: Option<PathBuf>,
    scope: &'static str,
    /// The file's own `about`, when it has one.
    about: Option<String>,
    /// Why this file is not the answer to its own name: it will not parse, or
    /// something outranks it. Listed rather than hidden — a theme somebody
    /// wrote and cannot see is what sends them hunting for a typo Emma has
    /// already found.
    problem: Option<String>,
    /// Whether `/theme <name>` resolves to this entry.
    live: bool,
}

/// The two directories, in the order a name is looked for in them.
///
/// **Yours beats the project's**, following the harness's own argument for
/// skipping `~/.claude/`: a project cannot select a theme, so the only way a
/// repository could change your colours is by shadowing a name you had already
/// chosen, and this order is what closes that. The reverse — your file
/// shadowing the project's — is not a risk, it is the preference working.
fn theme_dirs(home: Option<&Path>, root: &Path) -> Vec<(&'static str, PathBuf)> {
    let mut dirs = Vec::new();
    if let Some(home) = home {
        dirs.push(("yours", home.join(".emma").join("themes")));
    }
    // The harness root is already `.emma/` or `.claude/`, whichever this
    // directory has, so themes ride the discovery that has already happened
    // rather than getting a second rule to be wrong about.
    dirs.push(("this project", root.join("themes")));
    dirs
}

/// Everything nameable, built-in first, each directory sorted.
fn available(home: Option<&Path>, root: &Path) -> Vec<Found> {
    let mut all = vec![Found {
        name: BUILT_IN.to_string(),
        path: None,
        scope: "built-in",
        about: Some("what Emma looks like out of the box".to_string()),
        problem: None,
        live: true,
    }];
    for (scope, dir) in theme_dirs(home, root) {
        let mut here: Vec<Found> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .map(|e| {
                let path = e.path();
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let (about, problem) = peek(&path);
                Found {
                    name,
                    path: Some(path),
                    scope,
                    about,
                    problem,
                    live: false,
                }
            })
            .collect();
        here.sort_by(|a, b| a.name.cmp(&b.name));
        for mut f in here {
            f.live = f.problem.is_none() && !all.iter().any(|g| g.name == f.name);
            if !f.live && f.problem.is_none() {
                f.problem = Some(if f.name == BUILT_IN {
                    format!("ignored — `{BUILT_IN}` is the built-in; copy it to another name")
                } else {
                    "ignored — your own theme of this name wins".to_string()
                });
            }
            all.push(f);
        }
    }
    all
}

/// What a theme file says about itself, and whether it can be read at all.
///
/// Deliberately only the outermost shape: this is the failure that would turn a
/// selection into a boot-time fallback, and it is the one worth refusing over.
/// A bad hex on one role is not — the loader applies the rest of the file and
/// says so, which is a better answer than refusing the whole theme.
fn peek(path: &Path) -> (Option<String>, Option<String>) {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => return (None, Some(format!("cannot be read: {e}"))),
    };
    let doc: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(doc) => doc,
        Err(e) => return (None, Some(format!("is not JSON: {e}"))),
    };
    match doc.as_object() {
        Some(obj) => (
            obj.get("about")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            None,
        ),
        None => (None, Some("is not a JSON object".to_string())),
    }
}

fn theme(s: &Session<'_, '_>, name: Option<String>) {
    let home = s.home.as_deref();
    let root = s.harness.root.as_path();
    let all = available(home, root);
    let selected = home.and_then(|h| crate::settings::load(h).theme);

    // The same listing whether it was asked for or is the answer to a name that
    // is not there, because "what can I choose" is the question in both cases.
    let listing = || {
        list(
            &all,
            selected.as_deref(),
            s.theme_at_start.as_deref(),
            home,
            root,
        )
        .join("\n")
    };

    let Some(name) = name else {
        say(s.term, &listing());
        return;
    };

    let Some(entry) = all.iter().find(|f| f.live && f.name == name) else {
        // Nothing is written on this path, which is the point of doing the
        // lookup before the write rather than after it.
        s.term.warn(&refusal(&name, &all));
        say(s.term, &listing());
        return;
    };

    let Some(home) = home else {
        s.term.warn(
            "a theme is remembered in settings.json under your home directory, and this \
             platform did not give one — so there is nowhere to write the selection.",
        );
        return;
    };

    // Loaded before it is saved. The notices are the loader's own — a role name
    // that is a typo, a hex that is not one — and they are worth hearing at the
    // moment the file is chosen rather than only at the next boot.
    let (_, notices) = crate::term::theme::load(Some(home), Some(root), Some(&name));

    let path = match write_theme(home, &name) {
        Ok(path) => path,
        Err(e) => {
            s.term
                .warn(&format!("the selection could not be written: {e}"));
            return;
        }
    };

    let mut lines = vec![
        match &entry.path {
            Some(file) => format!("theme     {name}  ·  {}", file.display()),
            None => format!("theme     {name}  ·  the built-in"),
        },
        format!("written   {}", path.display()),
    ];
    let showing = s.theme_at_start.as_deref().unwrap_or(BUILT_IN);
    lines.push(if showing == name {
        "effect    already what this session started with, so nothing on screen changes."
            .to_string()
    } else {
        format!(
            "effect    the next start. A theme is read once, when Emma starts, so this screen\n\
             \x20         keeps {showing} until you leave and come back."
        )
    });
    for notice in notices {
        lines.push(format!("note      {notice}"));
    }
    if s.term.colour_level() == crate::term::palette::Level::None {
        // Said rather than refused: somebody configuring a machine they will
        // use from a colour terminal later is doing a reasonable thing, and the
        // preference is still worth storing. What would be wrong is letting
        // them believe a restart will show it.
        lines.push(
            "note      this run has no colour at all — NO_COLOR, EMMA_COLORS, or output that\n\
             \x20         is not a terminal — and that outranks every theme. Until that\n\
             \x20         changes, restarting will not look any different."
                .to_string(),
        );
    }
    say(s.term, &lines.join("\n"));
}

/// `/theme <name>` for a name that is not there.
///
/// It refuses rather than keeping the current theme quietly, and it lists —
/// `cli.rs`'s rule that a hint must be executable by the person reading it,
/// applied to the case where the whole problem is not knowing what exists.
fn refusal(name: &str, all: &[Found]) -> String {
    let live: Vec<&str> = all
        .iter()
        .filter(|f| f.live)
        .map(|f| f.name.as_str())
        .collect();
    let broken = all.iter().find(|f| f.name == name && f.problem.is_some());
    match broken {
        Some(f) => format!(
            "`{name}` {} — so nothing was changed. What can be selected: {}.",
            f.problem.as_deref().unwrap_or_default(),
            live.join(", ")
        ),
        None => format!(
            "there is no theme called `{name}`, so nothing was changed. What can be \
             selected: {}.",
            live.join(", ")
        ),
    }
}

/// The listing, and the empty case is the ordinary one.
///
/// A fresh machine has no theme files at all, so "nothing to show" is what most
/// people will see first and it has to answer the next question by itself:
/// what is running, where a file would go, and what the smallest one looks
/// like. `commands.rs`'s `init` and the `/` menu both set that precedent — an
/// empty list explains itself rather than showing nothing.
fn list(
    all: &[Found],
    selected: Option<&str>,
    at_start: Option<&str>,
    home: Option<&Path>,
    root: &Path,
) -> Vec<String> {
    let selected = selected.unwrap_or(BUILT_IN);
    let showing = at_start.unwrap_or(BUILT_IN);
    let mut lines = vec![format!("selected  {selected}")];
    if showing != selected {
        lines.push(format!(
            "on screen {showing}  ·  a theme is read once, when Emma starts — restart to see\n\
             \x20         {selected}."
        ));
    }
    lines.push(String::new());
    if all.len() == 1 {
        lines.push(format!(
            "available {BUILT_IN} (built-in), and nothing else: no theme files were found."
        ));
    } else {
        lines.push("available".to_string());
        for f in all {
            let note = f
                .problem
                .as_deref()
                .or(f.about.as_deref())
                .unwrap_or_default();
            // `live` as well as the name: two rows can carry one name — a
            // shadowed file, or one called after the built-in — and marking
            // both as selected points at the file that is being ignored. Found
            // by looking at a real listing, not by a test.
            let mark = if f.live && f.name == selected {
                ">"
            } else {
                " "
            };
            lines.push(
                format!("{mark} {:<14} {:<14} {note}", f.name, f.scope)
                    .trim_end()
                    .to_string(),
            );
        }
    }
    lines.push(String::new());
    lines.push("where".to_string());
    for (scope, dir) in theme_dirs(home, root) {
        lines.push(format!(
            "  {:<14} {}",
            scope,
            dir.join("<name>.json").display()
        ));
    }
    lines.push(format!(
        "  yours wins if both have the name, and `{BUILT_IN}` is the built-in — a file of\n\
         \x20 that name is ignored. Everything in a theme is optional: the smallest one\n\
         \x20 that works is {{\"roles\": {{\"accent\": \"#f5548f\"}}}}."
    ));
    lines.push(String::new());
    lines.push(
        "select    /theme <name>. It is written to settings.json — there is no --save —\n\
         \x20         and it is the next start that shows it."
            .to_string(),
    );
    lines
}

/// The one place the selection is written, and it is a round trip rather than a
/// rewrite: `settings.json` also holds the provider, a model per provider, the
/// validation block and the user-tool block, and every one of them has to
/// survive somebody changing their colours. Same shape, same reason, as
/// `commands::write_model`.
///
/// `pub(crate)` for the Settings screen's Theme row, which is the second
/// surface that selects a theme and must write it the same way — two round
/// trips over one file, minted independently, is how a settings key comes to be
/// dropped by whichever surface was written second.
pub(crate) fn write_theme(home: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let mut settings = crate::settings::load(home);
    settings.theme = Some(name.to_string());
    crate::settings::save(home, &settings)
}

// endregion: /theme

// region: /compact and /clear

fn compact(s: &mut Session<'_, '_>, everything: bool, instruction: Option<String>) {
    if let Some(words) = instruction {
        // Said rather than ignored. `Agent::compact`'s doc rules that
        // compaction is not model-summarised — a call there spends the running
        // goal's budget, can fail mid-goal, and produces a *claim* about the
        // conversation where the current code produces a *record* of it. An
        // instruction can only be honoured by a model, so the honest answer is
        // to say so and name the workflow that does work.
        say(
            s.term,
            &format!(
                "compaction replaces each finished goal with its goal text and its final \
                 answer. It does not call a model, so it cannot follow an instruction — \
                 \"{words}\" would be silently ignored, which is worse than saying so.\n\
                 Run /compact on its own, or state what to keep as an ordinary goal first \
                 (\"summarise what we established about the API\") — the answer of a goal \
                 always survives compaction, so it will still be there afterwards."
            ),
        );
        return;
    }
    match s.agent.compact_now(everything) {
        Compacted::Nothing(why) => s.term.note(&format!("nothing to compact — {why}")),
        Compacted::Done {
            goals,
            messages,
            before,
            after,
        } => say(
            s.term,
            &format!(
                "compacted {messages} messages from {goals} finished goal(s) — roughly {before} \
                 tokens down to {after}. Their tool results are no longer in context.\n\
                 The cached prefix is gone; the next call re-reads what is left at full price."
            ),
        ),
    }
}

/// `/clear` — a fresh conversation without leaving the session.
///
/// **The grants are kept, and they are named.** A session grant is consent
/// about the *process*: `[a]` reads "allow that tool for the rest of the
/// process, and no longer", and the network `[y]` says "this process only".
/// Neither is phrased in terms of the conversation and neither should quietly
/// acquire a second meaning — and clearing them makes nothing safer, since the
/// human is still at the keyboard and would simply be asked the same questions
/// again. What silence would cost is the user's ability to notice, so the
/// receipt says exactly what survived and that `/exit` is what drops it.
async fn clear(s: &mut Session<'_, '_>) {
    let (tools, hosts) = s.approvals.session_grants().await;
    let cleared = s.agent.clear();
    let mut lines = vec![format!(
        "cleared   {} goal(s), {} messages. The next goal starts a fresh conversation.",
        cleared.goals, cleared.messages
    )];
    if tools.is_empty() && hosts.is_empty() {
        lines.push("kept      no tool or host grants have been given this session".into());
    } else {
        let mut what = Vec::new();
        if !tools.is_empty() {
            what.push(format!(
                "{} ({})",
                plural(tools.len(), "tool grant"),
                tools.join(", ")
            ));
        }
        if !hosts.is_empty() {
            what.push(format!(
                "{} ({})",
                plural(hosts.len(), "host grant"),
                hosts.join(", ")
            ));
        }
        lines.push(format!(
            "kept      {} from this session — you will not be asked about them again until \
             you /exit",
            what.join(" and ")
        ));
    }
    lines.push(format!(
        "kept      the transcript: {} — a --resume will not replay what was cleared",
        s.log_path.display()
    ));
    lines.push(format!(
        "kept      {}, and this session's id",
        s.provider.model_id()
    ));
    say(s.term, &lines.join("\n"));
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("{n} {what}")
    } else {
        format!("{n} {what}s")
    }
}

// endregion: /compact and /clear

// region: Saying it
// ---------------------------------------------------------------------------
// Saying it
//
// One line per `Term::note`, rather than one note containing newlines: `note`
// renders through `Skin::note` and, on the framed path, `frame.write_lines`,
// and one line per row is what `insert_before` scrolls correctly.
// ---------------------------------------------------------------------------

/// What `/export` takes.
const EXPORT_USAGE: &str = "  /export            write this conversation beside the session log\n                            \x20 /export <path>     write it to that file instead";

/// Write the conversation to a file, in markdown.
///
/// **The file is the honest half of the copy story.** A clipboard write is an
/// escape sequence and is therefore forbidden on `-p`, on a pipe and with no
/// console — which is exactly where somebody is most likely to want the text out
/// and least able to select it. A file needs no escape byte, so this path is
/// available everywhere.
///
/// Rendered from the session log rather than from the transcript, for the same
/// reason `/copy` is: the log holds what was written, and the screen holds what
/// was painted beside a sidebar at some width.
fn export_conversation(s: &Session<'_, '_>, path: Option<&str>) {
    let (records, lost) = match crate::session::SessionLog::read_reporting(&s.log_path) {
        Ok(r) => r,
        Err(e) => {
            s.term
                .warn(&format!("/export could not read this session: {e:#}"));
            return;
        }
    };

    let target = match path {
        Some(p) => std::path::PathBuf::from(p),
        // Beside the log, named after it: the session id is already the name
        // somebody would reach for, and putting it in the working directory
        // would drop a file into a repository the user is editing.
        None => s.log_path.with_extension("md"),
    };

    let mut out = String::new();
    let mut turns = 0usize;
    for r in &records {
        match r["kind"].as_str().unwrap_or_default() {
            "goal" => {
                let text = r["text"].as_str().unwrap_or_default().trim();
                if !text.is_empty() {
                    out.push_str(&format!("\n## {text}\n\n"));
                    turns += 1;
                }
            }
            "assistant" => {
                let text = r["text"].as_str().unwrap_or_default().trim();
                if !text.is_empty() {
                    out.push_str(text);
                    out.push_str("\n\n");
                    turns += 1;
                }
            }
            _ => {}
        }
    }

    if turns == 0 {
        s.term
            .note("/export: nothing to write — this session has no conversation yet.");
        return;
    }

    // **A damaged log is said out loud, in the file and on screen.** `read_reporting`
    // returns the lines it could not parse, and an export that quietly omitted
    // them would produce a transcript that reads as complete. That is the exact
    // failure the session log's own loss counting exists to prevent, and it
    // would be undone here by not passing it on.
    if !lost.is_empty() {
        let note = format!(
            "> **{} record(s) of this session could not be read and are missing below** \
             (line {}). This export is incomplete.\n\n",
            lost.len(),
            lost.iter()
                .take(5)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        out.insert_str(0, &note);
    }

    match std::fs::write(&target, &out) {
        Ok(()) => {
            s.term.note(&format!(
                "/export: wrote {turns} turn(s) to {}",
                target.display()
            ));
            if !lost.is_empty() {
                s.term.warn(&format!(
                    "/export: {} record(s) could not be read and are missing from the file; \
                     it says so at the top.",
                    lost.len()
                ));
            }
        }
        Err(e) => s.term.warn(&format!(
            "/export could not write {}: {e}",
            target.display()
        )),
    }
}

/// The clipboard write, and the reasons it is shaped this way.
///
/// **OSC 52** — `ESC ] 52 ; c ; <base64> BEL` — asks the *terminal* to put text
/// on the user's clipboard. It is the only mechanism that works the same way
/// locally and over SSH, and it needs no dependency. Windows Terminal has
/// supported it since 2020.
///
/// **It is an escape sequence, so it is refused where escape sequences are.**
/// `INV-001` promises zero escape bytes on `-p`, on a pipe, under
/// `EMMA_NO_FRAME` and with no console. A clipboard write on any of those paths
/// would put raw bytes into somebody's redirected output, which is the exact
/// defect that invariant exists to prevent — so the check is on `Term::framed`
/// rather than on whether the write would probably be seen.
///
/// **Emma cannot tell whether it worked**, and says so instead of implying it
/// did. The terminal either honours the sequence or ignores it, silently, with
/// no reply; there is no acknowledgement in the protocol to wait for. Reporting
/// "copied" as a fact would be a claim about somebody else's program.
/// What is said when the clipboard write is refused rather than merely
/// unexplained. Shared by both arms so the two cannot drift apart.
const NO_ESCAPE_BYTES_HERE: &str =
    "/copy: nothing was sent — this run emits no escape bytes at all. Use `/export` instead.";

fn copy_last_answer(s: &Session<'_, '_>) {
    if !s.term.framed() {
        s.term.warn(
            "/copy writes to the clipboard with an escape sequence, which this run must not \
             emit — piped and `-p` output carries no escape bytes at all. Use `/export` to \
             write the transcript to a file instead.",
        );
        return;
    }

    let (records, _lost) = match crate::session::SessionLog::read_reporting(&s.log_path) {
        Ok(r) => r,
        Err(e) => {
            s.term
                .warn(&format!("/copy could not read this session: {e:#}"));
            return;
        }
    };
    // The last assistant turn that actually said something. A turn that only
    // called tools has no prose to copy, and skipping past it is what makes
    // `/copy` mean "the answer" rather than "the last record".
    let text = records
        .iter()
        .rev()
        .filter(|r| r["kind"] == "assistant")
        .find_map(|r| {
            let t = r["text"].as_str().unwrap_or_default().trim();
            (!t.is_empty()).then(|| t.to_string())
        });
    let Some(text) = text else {
        s.term
            .note("/copy: nothing to copy — this session has no answer yet.");
        return;
    };

    // The report follows the return value rather than the call, so it cannot
    // say "sent" about bytes that were never written. `clipboard` refuses on an
    // unframed run for `INV-001`; the guard at the top of this function exists
    // to explain that where there is somebody to read it, and this arm is what
    // remains if that guard is ever deleted.
    if !s.term.clipboard(&text) {
        s.term.warn(NO_ESCAPE_BYTES_HERE);
        return;
    }
    let lines = text.lines().count();
    let chars = text.chars().count();
    s.term.note(&format!(
        "/copy: sent {chars} characters ({lines} line(s)) to the clipboard. Emma cannot see \
         whether the terminal accepted it — if nothing arrives, this terminal does not do OSC 52."
    ));
}

fn say(term: &Term, text: &str) {
    for line in text.lines() {
        term.note(line);
    }
}

/// Run one of the writer-shaped `commands::*` into a string.
///
/// The CLI hands them `std::io::stdout()`; inside a session the same bytes have
/// to reach `Term`, or the framed path would print underneath its own viewport.
/// One implementation, two doors.
fn capture(f: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>) -> anyhow::Result<String> {
    let mut buf: Vec<u8> = Vec::new();
    f(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// endregion: Saying it

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_is_reachable_by_the_name_the_menu_shows() {
        // The menu builds its rows from `BUILTINS` and sends `/{name}`. A name
        // in that list that `parse` does not recognise is a row that does
        // nothing, which is the defect `menu.rs` calls "a command nobody can
        // discover" wearing the other face.
        for (name, _) in BUILTINS {
            assert!(
                parse(&format!("/{name}")).is_some(),
                "the menu offers /{name} and nothing parses it"
            );
        }
    }

    #[test]
    fn the_two_ways_out_are_ordinary_commands() {
        // They were a `matches!` bolted in front of everything else, which is
        // one more place the exit can be broken by an edit somewhere near it.
        assert_eq!(parse("/exit"), Some(SessionCommand::Exit));
        assert_eq!(parse("/quit"), Some(SessionCommand::Exit));
        assert_eq!(parse("  /exit  "), Some(SessionCommand::Exit));
        assert_eq!(parse("/EXIT"), Some(SessionCommand::Exit));
    }

    #[test]
    fn a_near_miss_is_not_a_command() {
        // Every one of these is either somebody's project command or somebody's
        // goal, and intercepting it would refuse work they asked for with no way
        // to insist. `cli.rs` has already paid for the opposite mistake.
        for line in [
            "/models",
            "/clearance",
            "/helper",
            "/exits",
            "model",
            "clear the build directory",
            "/usr/bin/env",
            "/review src/lib.rs",
            "",
            "/",
        ] {
            assert_eq!(parse(line), None, "`{line}` was intercepted");
        }
    }

    #[test]
    fn compact_tells_all_from_an_instruction_it_cannot_honour() {
        assert_eq!(
            parse("/compact"),
            Some(SessionCommand::Compact {
                everything: false,
                instruction: None
            })
        );
        assert_eq!(
            parse("/compact all"),
            Some(SessionCommand::Compact {
                everything: true,
                instruction: None
            })
        );
        // The words are carried, not dropped: they are what the refusal quotes.
        assert_eq!(
            parse("/compact keep the API details"),
            Some(SessionCommand::Compact {
                everything: false,
                instruction: Some("keep the API details".into())
            })
        );
    }

    #[test]
    fn model_takes_an_id_a_save_or_both_and_refuses_a_sentence() {
        assert_eq!(
            parse("/model"),
            Some(SessionCommand::Model {
                id: None,
                save: false
            })
        );
        assert_eq!(
            parse("/model claude-sonnet-4-5"),
            Some(SessionCommand::Model {
                id: Some("claude-sonnet-4-5".into()),
                save: false
            })
        );
        assert_eq!(
            parse("/model claude-sonnet-4-5 --save"),
            Some(SessionCommand::Model {
                id: Some("claude-sonnet-4-5".into()),
                save: true
            })
        );
        assert_eq!(
            parse("/model --save"),
            Some(SessionCommand::Model {
                id: None,
                save: true
            })
        );
        assert!(matches!(
            parse("/model the api surface"),
            Some(SessionCommand::Misuse { name: "model", .. })
        ));
    }

    /// The half-applied change is the failure worth a test: a provider swapped
    /// while the status row, `main`'s binding and the next `/config` still name
    /// the old id. Refusing before anything is touched is how that is prevented,
    /// so what is pinned is that the refusal happens on the argument alone.
    #[test]
    fn an_argument_that_cannot_be_a_model_id_is_refused_with_a_reason() {
        for bad in ["src/lib.rs", "-p", "claude opus", "", "claude/opus"] {
            let refusal = refuse_a_bad_id(bad).unwrap_or_else(|| panic!("`{bad}` was accepted"));
            assert!(refusal.contains("nothing was changed"), "{refusal}");
        }
        // …and a real id, including one this build has never heard of, is not.
        for good in [
            "claude-opus-5",
            "claude-3-5-sonnet-20241022",
            "claude-opus-9",
            "us.anthropic.claude-x",
        ] {
            assert_eq!(refuse_a_bad_id(good), None, "{good} was refused");
        }
    }

    /// The line that stops a silent degradation being diagnosed as "Emma got
    /// worse": an unknown model clamps to 8,192 output tokens and drops the
    /// effort parameter, and neither is visible anywhere else.
    #[test]
    fn what_a_model_accepts_names_the_floor_an_unknown_id_falls_to() {
        let unknown = accepts("claude-not-a-model-9");
        assert!(unknown.contains("8192"), "{unknown}");
        assert!(unknown.contains("no effort parameter"), "{unknown}");
        let known = accepts("claude-opus-5");
        assert!(known.contains("128000"), "{known}");
        assert!(known.contains("effort up to"), "{known}");
    }

    // region: /theme

    /// A tiny machine: a home with `~/.emma/themes/` and a harness root with
    /// `themes/`, so every rule about which of the two wins is exercised
    /// against real files rather than against a mock of the filesystem.
    struct Machine {
        home: tempfile::TempDir,
        project: tempfile::TempDir,
    }

    impl Machine {
        fn new() -> Self {
            let m = Self {
                home: tempfile::tempdir().unwrap(),
                project: tempfile::tempdir().unwrap(),
            };
            std::fs::create_dir_all(m.home.path().join(".emma").join("themes")).unwrap();
            std::fs::create_dir_all(m.root().join("themes")).unwrap();
            m
        }
        fn root(&self) -> PathBuf {
            self.project.path().join(".emma")
        }
        fn mine(&self, name: &str, body: &str) {
            std::fs::write(
                self.home
                    .path()
                    .join(".emma")
                    .join("themes")
                    .join(format!("{name}.json")),
                body,
            )
            .unwrap();
        }
        fn theirs(&self, name: &str, body: &str) {
            std::fs::write(
                self.root().join("themes").join(format!("{name}.json")),
                body,
            )
            .unwrap();
        }
        fn available(&self) -> Vec<Found> {
            available(Some(self.home.path()), &self.root())
        }
    }

    #[test]
    fn theme_takes_a_name_and_says_why_there_is_no_save_flag() {
        assert_eq!(parse("/theme"), Some(SessionCommand::Theme { name: None }));
        assert_eq!(
            parse("/theme oxide"),
            Some(SessionCommand::Theme {
                name: Some("oxide".into())
            })
        );
        // The flag somebody types out of `/model` habit gets a sentence rather
        // than being quietly swallowed and doing nothing.
        assert!(matches!(
            parse("/theme oxide --save"),
            Some(SessionCommand::Misuse { name: "theme", .. })
        ));
        assert!(matches!(
            parse("/theme --save"),
            Some(SessionCommand::Misuse { name: "theme", .. })
        ));
        assert!(THEME_USAGE.contains("no --save"), "{THEME_USAGE}");
    }

    /// `/export` takes a path or nothing, and a flag is refused rather than
    /// guessed at.
    ///
    /// A wrong guess here writes somebody's conversation to a file they did not
    /// name, which is the one mistake this command can make that the user cannot
    /// undo by running it again.
    #[test]
    fn export_takes_a_path_or_nothing() {
        assert_eq!(parse("/export"), Some(SessionCommand::Export(None)));
        assert_eq!(
            parse("/export out.md"),
            Some(SessionCommand::Export(Some("out.md".into())))
        );
        assert!(matches!(
            parse("/export --force"),
            Some(SessionCommand::Misuse { name: "export", .. })
        ));
    }

    /// The rule that closes the only door a repository has to your colours.
    #[test]
    fn your_own_theme_shadows_the_projects_and_the_built_in_name_is_reserved() {
        let m = Machine::new();
        m.mine("oxide", r#"{"about":"mine"}"#);
        m.theirs("oxide", r#"{"about":"theirs"}"#);
        m.theirs("house", r#"{"about":"the project's"}"#);
        m.theirs(
            BUILT_IN,
            r#"{"about":"a repository repainting stock Emma"}"#,
        );
        let all = m.available();

        let live: Vec<(&str, &str)> = all
            .iter()
            .filter(|f| f.live)
            .map(|f| (f.name.as_str(), f.scope))
            .collect();
        assert_eq!(
            live,
            vec![
                (BUILT_IN, "built-in"),
                ("oxide", "yours"),
                ("house", "this project")
            ]
        );
        // The loser is listed, with the reason — a file somebody wrote that is
        // simply absent from the list is what sends them hunting for a typo.
        let shadowed = all
            .iter()
            .find(|f| f.name == "oxide" && f.scope == "this project")
            .unwrap();
        assert!(
            shadowed.problem.as_deref().unwrap().contains("your own"),
            "{shadowed:?}"
        );
        let reserved = all
            .iter()
            .find(|f| f.name == BUILT_IN && f.path.is_some())
            .unwrap();
        assert!(
            reserved.problem.as_deref().unwrap().contains("built-in"),
            "{reserved:?}"
        );

        // And the marker in the listing points at the one that is in force, not
        // at every row sharing its name — the defect a real listing showed and
        // the unit tests had not: two `emma` rows, both marked selected, one of
        // them the file being ignored.
        let text = list(&all, None, None, Some(m.home.path()), &m.root());
        let marked: Vec<&String> = text.iter().filter(|l| l.starts_with('>')).collect();
        assert_eq!(marked.len(), 1, "{text:#?}");
        assert!(marked[0].contains("built-in"), "{marked:?}");
    }

    /// `BUILT_IN` here and `RESERVED` in `term::theme` are two spellings of one
    /// fact, and this is the assertion that stops them drifting: the loader
    /// itself must ignore a file under the name this module refuses to select.
    #[test]
    fn the_name_this_module_reserves_is_the_one_the_loader_reserves() {
        let m = Machine::new();
        m.mine(BUILT_IN, r##"{"roles":{"accent":"#010203"}}"##);
        let (theme, notices) =
            crate::term::theme::load(Some(m.home.path()), Some(&m.root()), Some(BUILT_IN));
        assert_eq!(theme, crate::term::theme::BUILTIN);
        assert!(
            notices.iter().any(|n| n.contains("built-in")),
            "{notices:?}"
        );
    }

    /// A file that will not parse is refused *before* anything is written.
    /// The loader would fall back with a notice rather than break the boot, so
    /// this is not about safety — it is about not writing a name into somebody's
    /// configuration that is already known to be wrong.
    #[test]
    fn a_file_that_is_not_a_json_object_is_listed_and_cannot_be_selected() {
        let m = Machine::new();
        m.mine("broken", "{ not json");
        m.mine("array", "[1, 2, 3]");
        m.mine("fine", r#"{}"#);
        let all = m.available();
        assert!(all.iter().any(|f| f.name == "fine" && f.live));
        for bad in ["broken", "array"] {
            let f = all.iter().find(|f| f.name == bad).unwrap();
            assert!(!f.live, "{bad} was selectable");
            assert!(f.problem.is_some(), "{bad} was listed with no reason");
        }
        let no = refusal("broken", &all);
        assert!(no.contains("nothing was changed"), "{no}");
        assert!(no.contains("is not JSON"), "{no}");
        // …and it still says what *can* be chosen, which is the whole point of
        // refusing out loud rather than keeping the current theme quietly.
        assert!(no.contains("fine"), "{no}");
        let missing = refusal("nowhere", &all);
        assert!(missing.contains("no theme called `nowhere`"), "{missing}");
        assert!(missing.contains("nothing was changed"), "{missing}");
        assert!(missing.contains("fine"), "{missing}");
    }

    /// The one that has already cost this project a stored key: a write path
    /// that replaces a shared configuration document instead of merging into
    /// it. Everything else in `settings.json` must survive a colour change.
    #[test]
    fn selecting_a_theme_preserves_every_other_key_in_settings() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".emma")).unwrap();
        std::fs::write(
            crate::settings::path(home.path()),
            r#"{"provider":"anthropic","models":{"anthropic":"claude-x","elsewhere":"model-y"},
                "validated":{"anthropic":{"at":"2026-08-11T09:14:22Z","how":"forced"}},
                "tools":{"editor":"C:\\Program Files\\odd name\\code.exe"}}"#,
        )
        .unwrap();

        let path = write_theme(home.path(), "oxide").unwrap();
        let back = crate::settings::load(home.path());
        assert_eq!(back.theme.as_deref(), Some("oxide"));
        assert_eq!(back.provider.as_deref(), Some("anthropic"));
        assert_eq!(back.models.get("anthropic").unwrap(), "claude-x");
        assert_eq!(back.models.get("elsewhere").unwrap(), "model-y");
        assert_eq!(
            back.tools.editor.as_deref(),
            Some("C:\\Program Files\\odd name\\code.exe")
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("forced"), "{raw}");

        // …and choosing again replaces the selection rather than accumulating.
        write_theme(home.path(), "house").unwrap();
        assert_eq!(
            crate::settings::load(home.path()).theme.as_deref(),
            Some("house")
        );
    }

    /// The empty case is the normal case — a fresh machine has no theme files
    /// at all — so the listing has to answer the next question by itself.
    #[test]
    fn an_empty_list_says_what_is_running_and_where_a_theme_file_would_go() {
        let m = Machine::new();
        let all = m.available();
        assert_eq!(all.len(), 1);
        let text = list(&all, None, None, Some(m.home.path()), &m.root()).join("\n");
        assert!(text.contains("no theme files were found"), "{text}");
        assert!(text.contains(BUILT_IN), "{text}");
        // Both directories, spelled out, and the smallest file that works.
        assert!(
            text.contains(
                &m.home
                    .path()
                    .join(".emma")
                    .join("themes")
                    .join("<name>.json")
                    .display()
                    .to_string()
            ),
            "{text}"
        );
        assert!(
            text.contains(
                &m.root()
                    .join("themes")
                    .join("<name>.json")
                    .display()
                    .to_string()
            ),
            "{text}"
        );
        assert!(text.contains("\"accent\""), "{text}");
        assert!(text.contains("/theme <name>"), "{text}");
    }

    /// After a selection the file and the screen disagree on purpose, and the
    /// listing is the only place that can say both. A list that reported one
    /// number for both would be telling somebody their new theme is already on.
    #[test]
    fn the_listing_separates_what_is_selected_from_what_is_on_screen() {
        let m = Machine::new();
        m.mine("oxide", r#"{"about":"warmer"}"#);
        let all = m.available();

        let changed = list(&all, Some("oxide"), None, Some(m.home.path()), &m.root()).join("\n");
        assert!(changed.contains("selected  oxide"), "{changed}");
        assert!(changed.contains("on screen emma"), "{changed}");
        assert!(changed.contains("restart"), "{changed}");
        assert!(changed.contains("warmer"), "{changed}");

        // Settled again: one fact, said once.
        let settled = list(
            &all,
            Some("oxide"),
            Some("oxide"),
            Some(m.home.path()),
            &m.root(),
        )
        .join("\n");
        assert!(!settled.contains("on screen"), "{settled}");
    }

    /// `NO_COLOR` outranks every theme, by construction rather than by check:
    /// it produces `Level::None`, and the palette returns before a theme is
    /// consulted. This pins the property the command's own message depends on —
    /// a selection changes not one colour on such a terminal.
    #[test]
    fn a_theme_changes_nothing_at_all_on_a_terminal_with_no_colour() {
        use crate::term::palette::{Level, Palette, Role};
        // Whatever a theme could possibly say, said as loudly as the schema
        // allows — including the sixteen-colour names, which are *inherited*
        // from the built-in role unless a file declares them. A fixture that
        // declared only hexes would agree with the built-in at every fidelity
        // below truecolor, and this test would then pass with the `Level::None`
        // short-circuit removed. It was written that way first and the
        // mutation walked straight through it.
        let m = Machine::new();
        m.mine(
            "loud",
            r##"{"roles":{
                 "accent":{"hex":"#010203","ansi256":21,"ansi16":"blue"},
                 "ok":{"hex":"#040506","ansi256":22,"ansi16":"cyan"},
                 "err":{"hex":"#070809","ansi256":23,"ansi16":"magenta"},
                 "warn":{"hex":"#0a0b0c","ansi256":24,"ansi16":"white"},
                 "info":{"hex":"#0d0e0f","ansi256":25,"ansi16":"green"},
                 "dim":{"hex":"#101112","ansi256":26,"ansi16":"yellow"},
                 "ground":{"hex":"#131415","ansi256":27,"ansi16":"red"}}}"##,
        );
        let (theme, notices) =
            crate::term::theme::load(Some(m.home.path()), Some(&m.root()), Some("loud"));
        assert!(notices.is_empty(), "{notices:?}");
        assert_ne!(
            theme,
            crate::term::theme::BUILTIN,
            "the fixture did nothing"
        );

        let plain = Palette::new(Level::None);
        let themed = Palette::with_theme(Level::None, theme);
        for role in [
            Role::Text,
            Role::Dim,
            Role::Ok,
            Role::Err,
            Role::Warn,
            Role::Info,
            Role::Accent,
            Role::Ground,
        ] {
            assert_eq!(
                themed.color(role),
                plain.color(role),
                "{role:?} changed under NO_COLOR"
            );
            assert_eq!(themed.style(role), plain.style(role), "{role:?}");
        }
        // …and the fixture really would have shown up anywhere else, which is
        // what makes the equalities above a result rather than a tautology.
        for level in [Level::Ansi16, Level::Ansi256, Level::Truecolor] {
            assert_ne!(
                Palette::with_theme(level, theme).color(Role::Accent),
                Palette::new(level).color(Role::Accent),
                "the fixture is indistinguishable from the built-in at {level:?}"
            );
        }
        assert_eq!(
            themed.chip(Role::Accent),
            plain.chip(Role::Accent),
            "the answer keys changed under NO_COLOR"
        );
    }

    // endregion: /theme

    #[test]
    fn the_resume_advice_names_the_command_that_actually_works() {
        // `cli.rs` refuses to ship a hint pointing at a dead command. This is
        // the same rule one level along: the sentence has to be executable by
        // the person reading it.
        assert!(RESUME_ADVICE.contains("emma --resume"), "{RESUME_ADVICE}");
        assert!(RESUME_ADVICE.contains("/exit"), "{RESUME_ADVICE}");
    }
}
