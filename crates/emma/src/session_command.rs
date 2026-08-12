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
        "config",
        "what this run resolved: harness, tools, rules, key",
    ),
    ("agents", "what each subagent type has cost and produced"),
    ("resume", "how to continue an earlier session"),
    ("exit", "end this session"),
    ("quit", "end this session — the same thing as /exit"),
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
// and because there is no picker to reuse yet. `design-provider-and-model.md`
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

    #[test]
    fn the_resume_advice_names_the_command_that_actually_works() {
        // `cli.rs` refuses to ship a hint pointing at a dead command. This is
        // the same rule one level along: the sentence has to be executable by
        // the person reading it.
        assert!(RESUME_ADVICE.contains("emma --resume"), "{RESUME_ADVICE}");
        assert!(RESUME_ADVICE.contains("/exit"), "{RESUME_ADVICE}");
    }
}
