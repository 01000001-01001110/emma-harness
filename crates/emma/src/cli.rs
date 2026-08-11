//! Argument parsing.
//!
//! Hand-rolled, and that is a choice rather than an omission. The flag set is
//! small, one flag turns off the only thing standing between a model and the
//! user's working tree, and the rule governing that one — that `--yes` is
//! reachable only alongside `-p` — is a cross-flag constraint a derive-based
//! parser would express as a runtime check anyway.

use std::path::PathBuf;
use std::time::Duration;

use emma_llm::Caching;

use crate::agent::Budgets;

// region: The help text
// ---------------------------------------------------------------------------
// The help text
//
// The only description of the approval model most people will read, which is
// why it states the gate's rules rather than just listing the flags.
// ---------------------------------------------------------------------------

pub const HELP: &str = "\
emma — an agent that holds a goal.

USAGE
  emma                         interactive: state a goal at the prompt
  emma goal \"<text>\"           state the goal on the command line
  emma -p \"<text>\"             one goal, no prompts, then exit
  emma api [<key>]             store an API key in ~/.emma (prompts, no echo)
  emma model [<name>]          show or set the default model
  emma init                    write a minimal working .emma/ here and stop
  emma config check            load .emma/ (or .claude/) and report; no model call
  emma --resume [<id>] [<text>]
                               continue the newest session started in this
                               directory, or the one named. Nothing is re-run:
                               the conversation comes back, the tools do not.

OPTIONS
  -p, --print                  non-interactive. Anything needing approval is
                               denied and the model is told why.
      --model <ID>             use this model for one run.
      --max-iterations <N>     model calls per goal (default 60).
      --max-tokens <N>         billable tokens per goal (default 500000).
      --timeout <SECONDS>      wall clock per goal (default 1800).
      --max-kicks <N>          times the loop may say 'not done, continue'
                               before giving up (default 3).
      --max-context <N>        how large one request may get before the
                               conversation behind it is compacted (default
                               120000). Per session, not per goal.
      --no-cache               do not send cache breakpoints.
      --session-dir <PATH>     where the JSONL transcript is written.
      --dangerously-skip-permissions
                               run every tool without asking. Loud, flag-only,
                               and never settable from configuration.
      --yes                    the same thing, accepted only with -p.
  -h, --help                   this.
  -V, --version                version.

THE INTERACTIVE SESSION
  A goal at the prompt runs until it is done or a budget stops it, then the
  prompt comes back.

  A session is one conversation. The next thing you type continues the last
  one — what was read, run and answered is still there, so a follow-up question
  does not re-read the file the answer came from. Budgets are still per goal;
  the conversation is not. When it grows past --max-context the oldest goals
  are compacted to their goal and their answer, and their tool results — file
  contents, command output — leave the conversation. Emma says so when it
  happens, and the transcript records exactly what was replaced.

  Between goals and during one:

    /exit, /quit               end the session.
    Ctrl-C                     interrupt the goal that is running.
    /<name>                    expand a command from commands/ in .emma/ (or
                               .claude/). `emma config check` lists the ones
                               this directory has; the session lists them at
                               startup. An unknown /word is just text.

  On a terminal that supports it, Emma frames the window: a status row on top,
  the transcript scrolling between, and the prompt pinned to the bottom row so
  an approval question cannot scroll away under the output that follows it.
  Anything else — a pipe, a redirect, a console without VT processing, or
  EMMA_NO_FRAME set — gets plain lines instead, with nothing else different.

APPROVAL
  Read, Glob and Grep run silently. Write, Edit and Bash ask, showing the
  command, the diff, or the path and size. Answering 'a' allows that one tool
  for the rest of the process and no longer — there is no permission that
  outlives the run. A PreToolUse hook that denies cannot be approved away.

  One exemption, by name: TaskCreate and TaskUpdate write, and never ask. They
  write only to the agent's own task file under .emma/, and a prompt every time
  the agent ticks off a task is a prompt that gets answered without being read —
  which costs the prompts on Write, Edit and Bash as well.
";

// endregion: The help text

// region: What a command line becomes
// ---------------------------------------------------------------------------
// What a command line becomes
//
// Command and options are separate because the options apply to a run and the
// command decides whether there is one. Four of the six commands never reach
// the loop at all.
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Option<String>),
    Api(Option<String>),
    Model(Option<String>),
    /// Write a harness in the working directory. Reaches no model, no key and
    /// no network, which is why `main.rs` handles it beside `api` and `model`
    /// rather than inside the runtime: the command that fixes "Emma will not
    /// start here" must not need Emma to start.
    Init,
    /// Continue a session. `session` is an id when one was named, and the goal
    /// is the ordinary positional text — a resume still needs something to
    /// work on, it simply starts with a conversation behind it.
    Resume {
        session: Option<String>,
        goal: Option<String>,
    },
    ConfigCheck,
    Help,
    Version,
}

#[derive(Debug)]
pub struct Opts {
    pub print: bool,
    pub skip_permissions: bool,
    pub model: Option<String>,
    pub caching: Caching,
    pub session_dir: Option<PathBuf>,
    pub budgets: Budgets,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            print: false,
            skip_permissions: false,
            model: None,
            caching: Caching::On,
            session_dir: None,
            budgets: Budgets::default(),
        }
    }
}

#[derive(Debug)]
pub struct Cli {
    pub command: Command,
    pub opts: Opts,
}

// endregion: What a command line becomes

// region: The parser
// ---------------------------------------------------------------------------
// The parser
//
// One pass, then the cross-flag rules at the end — `--yes` only with `-p`, and
// `-p` only with a goal. Both are checked after the loop rather than inside it
// because neither can be decided until every argument has been seen.
// ---------------------------------------------------------------------------

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut it = args.into_iter().peekable();
    let mut opts = Opts::default();
    let mut words: Vec<String> = Vec::new();
    let mut command: Option<Command> = None;
    let mut short_yes = false;

    while let Some(arg) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next()
                .ok_or_else(|| format!("{name} needs a value. See `emma --help`."))
        };
        match arg.as_str() {
            "-h" | "--help" => return done(Command::Help, opts),
            "-V" | "--version" => return done(Command::Version, opts),
            "-p" | "--print" => opts.print = true,
            "--dangerously-skip-permissions" => opts.skip_permissions = true,
            "--yes" => {
                opts.skip_permissions = true;
                short_yes = true;
            }
            "--no-cache" => opts.caching = Caching::Off,
            // Ambiguous only in principle: `--model X` is always the one-run
            // override wherever it appears, and `emma model X` — the bare word,
            // in first position — is the persisted setting. Different spellings,
            // different meanings, so position never has to disambiguate them,
            // and this arm needs no guard. There used to be a guarded copy of it
            // above this one; the guard was written for a distinction the
            // spellings already make, and it selected the same body.
            "--model" => opts.model = Some(value("--model")?),
            "--session-dir" => opts.session_dir = Some(PathBuf::from(value("--session-dir")?)),
            "--max-iterations" => {
                opts.budgets.max_iterations = number(&value("--max-iterations")?)?
            }
            "--max-tokens" => opts.budgets.max_tokens = number(&value("--max-tokens")?)?,
            "--max-kicks" => opts.budgets.max_kicks = number(&value("--max-kicks")?)?,
            "--max-context" => opts.budgets.max_context = number(&value("--max-context")?)?,
            "--timeout" => {
                opts.budgets.wall_clock = Duration::from_secs(number(&value("--timeout")?)?)
            }
            // Spelled as a flag rather than a subcommand because it modifies a
            // run — it takes the same goal text, the same budgets and the same
            // approval rules — where `api`, `model` and `init` replace one.
            "--resume" => {
                if !matches!(command, None | Some(Command::Run(_))) {
                    return Err("--resume cannot be combined with another command.".into());
                }
                // A session id or a goal, told apart by the fixed `sess-`
                // prefix `SessionLog::new_id` gives every id. The alternative
                // is a second flag for the id, which makes the common
                // spelling — bare `--resume` with a goal after it — the one
                // that needs explaining.
                let named = it.peek().is_some_and(|a| a.starts_with("sess-"));
                let session = if named { it.next() } else { None };
                command = Some(Command::Resume {
                    session,
                    goal: None,
                });
            }
            // Subcommands, recognised only in first position so that a goal
            // beginning with the word "model" is still a goal.
            "goal" if fresh(&command, &words) => command = Some(Command::Run(None)),
            "init" if fresh(&command, &words) => command = Some(Command::Init),
            "api" if fresh(&command, &words) => command = Some(Command::Api(None)),
            "model" if fresh(&command, &words) => command = Some(Command::Model(None)),
            "config" if fresh(&command, &words) => match it.next().as_deref() {
                Some("check") => command = Some(Command::ConfigCheck),
                Some(other) => {
                    return Err(format!(
                        "unknown: `config {other}`. Did you mean `config check`?"
                    ))
                }
                None => return Err("`config` needs a subcommand: `config check`.".into()),
            },
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`. See `emma --help`."))
            }
            other => words.push(other.to_string()),
        }
    }

    // The cross-flag rule, and the reason the short spelling exists at all.
    // `--yes` is a scripting convenience; typing it at an interactive prompt
    // must not be enough to turn the gate off, because the whole hazard of a
    // bypass is that it is easy to reach. The long name is a sentence nobody
    // types by accident.
    if short_yes && !opts.print {
        return Err(
            "--yes only applies to `-p` runs. Interactively, if you really want no \
                    approval prompts at all, use --dangerously-skip-permissions."
                .into(),
        );
    }

    let joined = (!words.is_empty()).then(|| words.join(" "));
    let command = match command {
        Some(Command::Run(_)) | None => Command::Run(joined),
        Some(Command::Api(_)) => Command::Api(joined),
        Some(Command::Model(_)) => Command::Model(joined),
        Some(Command::Resume { session, .. }) => Command::Resume {
            session,
            goal: joined,
        },
        // Refused rather than ignored: `emma init something` is somebody
        // expecting the word to mean something, and writing a harness while
        // silently discarding it is the wrong half of the guess.
        Some(Command::Init) => match joined {
            Some(extra) => {
                return Err(format!(
                    "`init` takes no arguments; got `{extra}`. It writes .emma/ in the \
                     current directory."
                ))
            }
            None => Command::Init,
        },
        Some(other) => other,
    };
    // `-p` cannot answer a prompt, and a resumed run needs a goal for the same
    // reason a fresh one does: the restored conversation is context, not an
    // instruction to continue.
    let needs_goal = matches!(
        command,
        Command::Run(None) | Command::Resume { goal: None, .. }
    );
    if needs_goal && opts.print {
        return Err("-p needs a goal: `emma -p \"make the tests pass\"`.".into());
    }
    Ok(Cli { command, opts })
}

// endregion: The parser

// region: A command typed at the goal prompt
// ---------------------------------------------------------------------------
// A command typed at the goal prompt
//
// The `>` prompt takes goals, and everything else Emma understands is a
// command line. Somebody who types one at the prompt has confused the two, and
// until now the loop was correct and expensive about it: `init` is a
// perfectly good goal, so the model went and found out what `init` does — nine
// model calls, 580,000 tokens, one `Bash` call running `emma.exe init` to read
// the refusal this file could have quoted.
//
// So the answer is given here, for free. The hazard is the opposite mistake:
// "initialise the auth module" is a real goal and intercepting it would refuse
// work somebody asked for, which is worse than the bug. Hence the matching is
// deliberately narrow — the *whole* trimmed line, optionally with a leading
// `emma`, against the vocabulary this file actually parses, with only the
// arguments those commands actually take.
// ---------------------------------------------------------------------------

/// What to print instead of spending a turn.
#[derive(Debug, PartialEq, Eq)]
pub enum Typed {
    /// Not a command. Send it to the model, unchanged.
    Goal,
    /// A command that runs before the session does. Nothing about running it
    /// now would be true, so the answer is what to do instead.
    Elsewhere(String),
    /// A command that costs nothing to answer here, already answered.
    Answer(String),
}

/// Recognise one of Emma's own command lines typed at the goal prompt.
///
/// The four setup commands are handled *before the async runtime* on purpose
/// (see `main.rs`), and by the time this prompt exists the harness, the model
/// and the key for this session are already resolved. Running one now would
/// either do nothing visible or change a file this process has finished
/// reading — so the honest answer is the sentence, not the side effect.
/// `--help` and `--version` are pure text and are simply printed; making
/// somebody leave the session to read the help is its own small insult.
pub fn typed_at_the_prompt(line: &str) -> Typed {
    let line = line.trim();
    // `emma init` pasted as if into a shell is at least as likely as bare
    // `init`, and `emma.exe` is what a Windows user copies out of their own
    // terminal history.
    let rest = line
        .strip_prefix("emma.exe ")
        .or_else(|| line.strip_prefix("emma "))
        .unwrap_or(line)
        .trim();
    let words: Vec<&str> = rest.split_whitespace().collect();
    // Flags are matched with their case intact, because `parse` matches them
    // that way: `-V` is the version and `-v` is nothing at all. Recognising a
    // spelling the real parser rejects would be inventing a command, and the
    // instruction here is to document what exists.
    match (words.first().copied(), words.len()) {
        (Some("-h" | "--help"), 1) => return Typed::Answer(HELP.to_string()),
        (Some("-V" | "--version"), 1) => {
            return Typed::Answer(format!("emma {}", env!("CARGO_PKG_VERSION")))
        }
        _ => {}
    }
    // Subcommands are words rather than flags, and `Init` at a prompt is the
    // same mistake as `init`. Nothing below is echoed back, so the original
    // spelling is not needed past this point.
    let head = words.first().map(|w| w.to_ascii_lowercase());
    let name = match (head.as_deref(), words.len()) {
        (Some("init"), 1) => "init",
        // One argument at most, and it is never repeated back: the argument to
        // `api` is an API key, and a key echoed into a terminal is a key in
        // somebody's scrollback and in this session's transcript.
        (Some("api"), 1 | 2) => "api",
        (Some("model"), 1 | 2) => "model",
        (Some("config"), 2) if words[1].eq_ignore_ascii_case("check") => "config check",
        // Everything else is a goal, including `init the database`, `model the
        // API surface` and every sentence that merely starts with one of these
        // words. The length checks above are what make that true.
        _ => return Typed::Goal,
    };
    Typed::Elsewhere(format!(
        "`{name}` is an emma command, not a goal — and it runs before a session starts. This one \
         already has its harness, model and key loaded, so running it now would either change \
         nothing you can see or change something this session would not pick up. Use /exit, then \
         run `emma {name}` in your shell."
    ))
}

// endregion: A command typed at the goal prompt

// region: The parser's helpers

/// A subcommand is only a subcommand in first position.
fn fresh(command: &Option<Command>, words: &[String]) -> bool {
    command.is_none() && words.is_empty()
}

fn done(command: Command, opts: Opts) -> Result<Cli, String> {
    Ok(Cli { command, opts })
}

fn number<T: std::str::FromStr>(raw: &str) -> Result<T, String> {
    raw.parse()
        .map_err(|_| format!("`{raw}` is not a number. See `emma --help`."))
}

// endregion: The parser's helpers

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The cross-flag rule and the subcommand-in-first-position rule get a test
// each, because both are the kind of thing a later refactor tidies away
// without noticing what it was for.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Cli, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn a_bare_run_has_no_goal_and_asks_for_approval() {
        let cli = p(&[]).unwrap();
        assert_eq!(cli.command, Command::Run(None));
        assert!(!cli.opts.print && !cli.opts.skip_permissions);
    }

    #[test]
    fn the_goal_can_be_stated_either_way() {
        assert_eq!(
            p(&["goal", "port the middleware"]).unwrap().command,
            Command::Run(Some("port the middleware".into()))
        );
        assert_eq!(
            p(&["port", "the", "middleware"]).unwrap().command,
            Command::Run(Some("port the middleware".into()))
        );
    }

    #[test]
    fn the_short_bypass_cannot_be_reached_interactively() {
        // The failure this prevents: `emma --yes` at a terminal, which reads as
        // "stop asking me" and means "run anything".
        let err = p(&["--yes"]).unwrap_err();
        assert!(err.contains("--dangerously-skip-permissions"), "{err}");
        assert!(p(&["-p", "--yes", "go"]).unwrap().opts.skip_permissions);
        assert!(
            p(&["--dangerously-skip-permissions"])
                .unwrap()
                .opts
                .skip_permissions
        );
    }

    #[test]
    fn print_without_a_goal_is_refused_rather_than_hanging_on_a_prompt() {
        assert!(p(&["-p"]).unwrap_err().contains("needs a goal"));
    }

    #[test]
    fn the_persisted_model_and_the_one_run_override_are_different_words() {
        assert_eq!(
            p(&["model", "claude-x"]).unwrap().command,
            Command::Model(Some("claude-x".into()))
        );
        assert_eq!(p(&["model"]).unwrap().command, Command::Model(None));
        let cli = p(&["--model", "claude-x", "goal", "do it"]).unwrap();
        assert_eq!(cli.opts.model.as_deref(), Some("claude-x"));
        assert_eq!(cli.command, Command::Run(Some("do it".into())));
    }

    #[test]
    fn the_key_may_be_an_argument_or_a_prompt() {
        assert_eq!(p(&["api"]).unwrap().command, Command::Api(None));
        assert_eq!(
            p(&["api", "sk-ant-x"]).unwrap().command,
            Command::Api(Some("sk-ant-x".into()))
        );
    }

    #[test]
    fn a_goal_may_begin_with_a_subcommands_word() {
        // `model` is a subcommand in first position and an ordinary word after
        // one, so this stays a goal rather than becoming a settings write.
        assert_eq!(
            p(&["goal", "model", "the", "api"]).unwrap().command,
            Command::Run(Some("model the api".into()))
        );
    }

    #[test]
    fn budgets_come_off_the_command_line() {
        let cli = p(&[
            "--max-iterations",
            "3",
            "--max-tokens",
            "99",
            "--timeout",
            "5",
            "--max-kicks",
            "0",
            "--max-context",
            "1234",
        ])
        .unwrap();
        assert_eq!(cli.opts.budgets.max_iterations, 3);
        assert_eq!(cli.opts.budgets.max_tokens, 99);
        assert_eq!(cli.opts.budgets.wall_clock.as_secs(), 5);
        assert_eq!(cli.opts.budgets.max_kicks, 0);
        assert_eq!(cli.opts.budgets.max_context, 1234);
    }

    #[test]
    fn resume_tells_a_session_id_from_a_goal_by_its_prefix() {
        // The distinction the `sess-` prefix buys: no second flag, and the
        // spelling somebody reaches for first — `--resume` plus what to do
        // next — means what it looks like.
        assert_eq!(
            p(&["--resume"]).unwrap().command,
            Command::Resume {
                session: None,
                goal: None
            }
        );
        assert_eq!(
            p(&["--resume", "sess-0000000000001-9"]).unwrap().command,
            Command::Resume {
                session: Some("sess-0000000000001-9".into()),
                goal: None
            }
        );
        assert_eq!(
            p(&["--resume", "finish", "the", "port"]).unwrap().command,
            Command::Resume {
                session: None,
                goal: Some("finish the port".into())
            }
        );
        // A resumed run is still a run, so it still cannot be unattended
        // without something to be unattended about.
        assert!(p(&["-p", "--resume"]).unwrap_err().contains("needs a goal"));
        assert!(p(&["api", "--resume"]).is_err());
    }

    #[test]
    fn init_is_a_first_position_subcommand_and_takes_nothing() {
        assert_eq!(p(&["init"]).unwrap().command, Command::Init);
        // The word after a goal is a word, not a command.
        assert_eq!(
            p(&["goal", "init", "the", "repo"]).unwrap().command,
            Command::Run(Some("init the repo".into()))
        );
        assert!(p(&["init", "please"]).unwrap_err().contains("no arguments"));
    }

    // -----------------------------------------------------------------------
    // A command typed at the goal prompt
    //
    // Two tests, and the second one is the important one. Missing an
    // interception costs tokens; making one that should not have happened
    // refuses work somebody asked for, and they have no way to insist.
    // -----------------------------------------------------------------------

    #[test]
    fn a_setup_command_typed_at_the_prompt_is_answered_rather_than_investigated() {
        // The run this is written from: `emma init` at the `>` prompt, nine
        // model calls, 36,363 paths globbed, one `Bash` call executing
        // `emma.exe init` to read a refusal, 580,000 tokens.
        for line in [
            "init",
            "emma init",
            "emma.exe init",
            "  emma init  ",
            "Init",
            "api",
            "api sk-ant-not-a-real-key",
            "model",
            "model claude-x",
            "config check",
            "emma config check",
        ] {
            match typed_at_the_prompt(line) {
                Typed::Elsewhere(say) => {
                    assert!(say.contains("/exit"), "{line}: {say}");
                    // The argument is never repeated. `api <key>` is the case
                    // that matters: an echoed key is a key in the scrollback
                    // and in the session transcript.
                    assert!(!say.contains("sk-ant"), "{line}: a key was echoed back");
                }
                other => panic!("`{line}` was not recognised: {other:?}"),
            }
        }
    }

    #[test]
    fn a_goal_that_merely_starts_with_a_command_word_is_still_a_goal() {
        // The false positive is the expensive mistake in the other direction:
        // every one of these is somebody's actual work, and there is no way
        // for them to overrule a refusal.
        for line in [
            "initialise the auth module",
            "init the database schema and seed it",
            "model the API surface",
            "config check the deploy script",
            "api", // handled above
            "write an api client",
            "emma should init a new crate",
            "",
        ] {
            let verdict = typed_at_the_prompt(line);
            if line == "api" {
                continue;
            }
            assert_eq!(verdict, Typed::Goal, "`{line}` was intercepted");
        }
    }

    #[test]
    fn help_and_version_are_answered_in_place() {
        // No reason to make somebody leave the session to read the help, and
        // the help is the only place the session's own vocabulary is written
        // down.
        match typed_at_the_prompt("--help") {
            Typed::Answer(text) => assert!(text.contains("/exit"), "{text}"),
            other => panic!("{other:?}"),
        }
        match typed_at_the_prompt("emma --version") {
            Typed::Answer(text) => assert!(text.starts_with("emma "), "{text}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(typed_at_the_prompt("-V"), Typed::Answer(_)));
        // `-v` is not a spelling `parse` accepts, so it is not one this
        // recognises either. Answering it here would document a flag that does
        // not exist — the same defect as a hint pointing at a dead command,
        // wearing a friendlier face.
        assert_eq!(typed_at_the_prompt("-v"), Typed::Goal);
    }

    #[test]
    fn the_help_documents_the_way_out_of_the_session() {
        // The defect: `/exit` and `/quit` have worked since the loop was
        // written and were documented nowhere a user looks, so the owner went
        // looking for the exit command and could not find one.
        assert!(HELP.contains("/exit"), "the help does not say how to leave");
        assert!(HELP.contains("/quit"));
        assert!(HELP.contains("Ctrl-C"));
    }

    #[test]
    fn typos_are_refused_rather_than_guessed_at() {
        assert_eq!(
            p(&["config", "check"]).unwrap().command,
            Command::ConfigCheck
        );
        assert!(p(&["config", "chekc"]).is_err());
        assert!(p(&["--nope"]).is_err());
        assert!(p(&["--model"]).is_err());
    }
}

// endregion: Tests
