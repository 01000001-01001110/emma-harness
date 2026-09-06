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

/// The command-line half. A macro rather than a `const` because `concat!` takes
/// literals and nothing else — which is the whole mechanism keeping [`HELP`] and
/// [`SESSION_HELP`] made of the same bytes rather than two texts that agree
/// until somebody edits one.
macro_rules! usage_and_options {
    () => {
        "\
emma — an agent that holds a goal.

USAGE
  emma                         interactive: state a goal at the prompt
  emma goal \"<text>\"           state the goal on the command line
  emma -p \"<text>\"             one goal, no prompts, then exit
  emma set-provider <name> [--key <KEY>] [--model <ID>]
                               store that provider's key in ~/.emma and make it
                               the one Emma uses. Prompts for the key without
                               echoing it unless --key or a pipe supplies one.
  emma set-model <id> [--provider <name>]
                               set the model for the current provider, or for
                               the one named. Each provider remembers its own.
  emma api [<key>]             alias: store a key for the current provider only
  emma model [<name>]          alias: show or set that provider's model
  emma init                    write a minimal working .emma/ here and stop
  emma config check            load .emma/ (or .claude/) and report; no model call
  emma agents                  what each subagent type has cost and produced,
                               across every recorded session; no model call
  emma verify [--rows <ids>] [--limit <n>] [--dry-run]
                               send an independent reviewer at each outstanding
                               row of verification/parity/ledger.json, briefed
                               to disprove it rather than confirm it, and write
                               a receipt with its verdict and its whole report.
                               **This one spends money**: each row is a model
                               run with tools. --limit defaults to 5; --dry-run
                               lists what it would review and reaches no model.
                               --model chooses the reviewer.
  emma --resume [<id>] [<text>]
                               continue the newest session started in this
                               directory, or the one named. Nothing is re-run:
                               the conversation comes back, the tools do not.

OPTIONS
  -p, --print                  non-interactive. Anything needing approval is
                               denied and the model is told why.
      --model <ID>             use this model for one run — or, with
                               set-provider, the model to store.
      --provider <NAME>        use this provider for one run — or, with
                               set-model, the provider to set it for.
      --key <KEY>              with set-provider: the key, for scripts. `-`
                               reads it from stdin.
      --no-verify, --force     accepted and currently redundant: nothing is
                               checked against the provider yet.
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
"
    };
}

/// The whole of `emma --help`.
/// How many rows one `emma verify` reviews unless told otherwise.
///
/// **Low on purpose.** Each row is an independent model run with tools, so a
/// default that swept the whole ledger would turn a curious first command into
/// a bill nobody chose. Five is enough to see whether the reviews are any good,
/// which is the question a first run is really asking.
pub const DEFAULT_VERIFY_LIMIT: usize = 5;

/// The whole of `emma --help`.
pub fn help() -> String {
    format!("{}\n{}", usage_and_options!(), session_help())
}

/// What a running session understands, and what the gate does: the bytes
/// `/help` prints, and the tail of [`help`].
///
/// **It was a string literal, and the page is why it is not.** The full-screen
/// Help page needs section headers, an entry column and a scroll offset, so
/// the text moved to [`crate::term::help::SECTIONS`] and both renderings read
/// it from there. The chords in it are resolved against the keymap in force at
/// the moment it is built, which a `const` cannot do, so `emma --help` prints
/// the chord this user actually has.
pub fn session_help() -> String {
    crate::term::help::plain()
}

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
    /// Store a provider's key and select it. The model, when one is named,
    /// arrives as `opts.model` — the same flag that overrides a run, because
    /// "--model means an id for this provider" is one rule rather than two.
    SetProvider {
        name: String,
        key: Option<String>,
    },
    /// Set the remembered model for a provider. Which provider is `opts.provider`,
    /// defaulting to the one currently selected.
    SetModel(String),
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
    /// Dispatch independent reviewers at the parity ledger's outstanding rows.
    ///
    /// **The one command here that spends money on purpose**, which is why
    /// `limit` defaults low and `dry_run` exists: a fan-out across ninety-odd
    /// rows is a real bill, and a flag that has to be typed is the difference
    /// between a decision and an accident.
    Verify {
        /// Named rows, or empty for "everything still wanting a review".
        rows: Vec<String>,
        /// How many rows to review in this run.
        limit: usize,
        /// List what would be reviewed and stop. Reaches no model.
        dry_run: bool,
        /// Print each selected row's brief and stop. Reaches no model.
        ///
        /// **So the reviewer can be something other than this binary.** The
        /// brief is the whole product of this command and it must have one
        /// source; a second copy in a shell script is how the fan-out and the
        /// gate came to disagree about what "reviewed" means. Print it, and let
        /// whoever is cheaper to run do the reading.
        print_brief: bool,
    },
    /// What delegation has actually cost. Reads the session transcripts and
    /// prints; calls no model, exactly as `config check` does not.
    Agents,
    Help,
    Version,
}

#[derive(Debug)]
pub struct Opts {
    pub print: bool,
    pub skip_permissions: bool,
    pub model: Option<String>,
    pub provider: Option<String>,
    /// Only `set-provider` reads it. It lives here rather than in the command
    /// so that the redaction rule — a key is never echoed back — has one place
    /// to be true of.
    pub key: Option<String>,
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
            provider: None,
            key: None,
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

/// The subcommand a single-word goal was probably meant to be.
///
/// Distance one, and one word only. Two edits reaches too far — `agent` is one
/// from `agents` and `test` is four from anything, which is the separation that
/// makes this safe to apply without asking. Case-insensitive, because `Init` is
/// the same mistake as `init` and `parse` matches the lowercase spelling.
fn near_miss(goal: &str) -> Option<&'static str> {
    const COMMANDS: &[&str] = &[
        "goal",
        "init",
        "api",
        "model",
        "set-provider",
        "set-model",
        "agents",
        "config",
        "verify",
    ];
    if goal.split_whitespace().count() != 1 {
        return None;
    }
    let g = goal.to_ascii_lowercase();
    // Equality is NOT excluded. `parse` matches subcommands case-sensitively, so
    // `Agents` never became one — it is a case typo and belongs here. Excluding
    // it was the first version's bug, found by its own test.
    COMMANDS
        .iter()
        .copied()
        .find(|c| edit_distance_at_most_one(&g, c))
}

/// Whether two strings are within one insertion, deletion, substitution — or
/// one transposition of adjacent characters.
///
/// Written out rather than pulling a crate: this is the whole of what is needed,
/// and a dependency for eight comparisons of short strings is a dependency to
/// audit, update and explain.
///
/// **The transposition arm is not a refinement; it is the common case.** This
/// was Levenshtein-only, under a doc that said "one edit" — and an adjacent
/// swap is Levenshtein distance *two*, so every transposed subcommand walked
/// straight past the guard and started a paid session. A reviewer certified
/// four on the release binary: `emma inti`, `emma modle`, `emma agnets`,
/// `emma confgi`.
///
/// `inti` is the transposition of `init`, which is the exact word whose misread
/// cost the run this guard was built after. The row's framing — "one word, one
/// edit" — concealed *which* edits, and the one it left out is the one fingers
/// actually make.
fn edit_distance_at_most_one(a: &str, b: &str) -> bool {
    if is_transposition(a, b) {
        return true;
    }

    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (long, short) = if a.len() >= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    if long.len() - short.len() > 1 {
        return false;
    }
    let mut i = 0;
    let mut j = 0;
    let mut slack = 1usize;
    while i < long.len() && j < short.len() {
        if long[i] == short[j] {
            i += 1;
            j += 1;
            continue;
        }
        if slack == 0 {
            return false;
        }
        slack -= 1;
        if long.len() == short.len() {
            i += 1;
            j += 1;
        } else {
            i += 1;
        }
    }
    // Whatever is left over must fit in the slack that is still unspent.
    slack >= (long.len() - i) + (short.len() - j)
}

/// Whether two strings differ by swapping one adjacent pair and nothing else.
///
/// Same length, exactly two differing positions, and those positions adjacent
/// and crossed. Deliberately narrow: `ab` vs `ba` yes, `abc` vs `cba` no. A
/// guard that refuses too much makes a legitimate one-word goal impossible to
/// type, and the whole point of this path is that it is cheap to be wrong in
/// only one direction.
fn is_transposition(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len() != b.len() {
        return false;
    }
    let diff: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
    matches!(diff[..], [i, j] if j == i + 1 && a[i] == b[j] && a[j] == b[i])
}

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
            "--provider" => opts.provider = Some(value("--provider")?),
            "--key" => opts.key = Some(value("--key")?),
            // Accepted now and redundant now: nothing in this build asks a
            // provider whether a key or a model is real, so every path is
            // already the unverified one. They parse so that a script written
            // against the documented flow keeps working when verification
            // lands, rather than failing on an unknown option.
            "--no-verify" | "--force" => {}
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
            "set-provider" if fresh(&command, &words) => {
                command = Some(Command::SetProvider {
                    name: String::new(),
                    key: None,
                })
            }
            "set-model" if fresh(&command, &words) => {
                command = Some(Command::SetModel(String::new()))
            }
            "agents" if fresh(&command, &words) => command = Some(Command::Agents),
            "verify" if fresh(&command, &words) => {
                command = Some(Command::Verify {
                    rows: Vec::new(),
                    limit: DEFAULT_VERIFY_LIMIT,
                    dry_run: false,
                    print_brief: false,
                })
            }
            // These three only mean anything to `verify`, and saying so beats
            // accepting them anywhere and ignoring them somewhere.
            other @ ("--rows" | "--limit" | "--dry-run" | "--print-brief")
                if !matches!(command, Some(Command::Verify { .. })) =>
            {
                return Err(format!(
                    "`{other}` is an option of `emma verify`. See `emma --help`."
                ))
            }
            "--rows" => {
                let Some(list) = it.next() else {
                    return Err("`--rows` needs a comma-separated list of row ids.".into());
                };
                if let Some(Command::Verify { rows, .. }) = command.as_mut() {
                    rows.extend(
                        list.split(',')
                            .map(|r| r.trim().to_ascii_uppercase())
                            .filter(|r| !r.is_empty()),
                    );
                }
            }
            "--limit" => {
                let Some(n) = it.next() else {
                    return Err("`--limit` needs a number.".into());
                };
                let Ok(n) = n.parse::<usize>() else {
                    return Err(format!("`--limit {n}` is not a number."));
                };
                if n == 0 {
                    return Err(
                        "`--limit 0` would review nothing. Omit it, or use --dry-run.".into(),
                    );
                }
                if let Some(Command::Verify { limit, .. }) = command.as_mut() {
                    *limit = n;
                }
            }
            "--dry-run" => {
                if let Some(Command::Verify { dry_run, .. }) = command.as_mut() {
                    *dry_run = true;
                }
            }
            "--print-brief" => {
                if let Some(Command::Verify { print_brief, .. }) = command.as_mut() {
                    *print_brief = true;
                }
            }
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

    // **A mistyped subcommand must not become a paid model call.**
    //
    // Every non-flag word falls through to a goal, so `emma agent` — one letter
    // short of `agents` — is not an error. It is a goal, and a goal starts a
    // session, calls the model and spends money answering a word the user never
    // meant as a question. Found by running `emma nonsense-subcommand` expecting
    // a refusal and watching it launch a real run: a shell, a `Glob *` that hit
    // the 1000-path cap, and several greps, all in service of a typo.
    //
    // The interactive prompt has been guarded against the neighbouring mistake
    // since a run that cost 580,000 tokens on a misread `init` — see
    // `typed_at_the_prompt`. The argv path had no equivalent, so the same hazard
    // was closed on one door and open on the other.
    //
    // **Narrow on purpose: one word, and close to a real command.** A goal is
    // usually a sentence; a single word that is one edit from a subcommand is
    // almost never one. Multi-word goals are untouched, and `emma goal init` is
    // the escape hatch for anybody who genuinely means the word — which is why
    // this refuses rather than guessing, and names the two ways forward.
    if matches!(command, None | Some(Command::Run(_))) {
        if let Some(goal) = joined.as_deref() {
            if let Some(meant) = near_miss(goal) {
                return Err(format!(
                    "`{goal}` is not an emma command, and as a goal it would start a session and \
                     call the model. Did you mean `emma {meant}`? If `{goal}` really is the goal, \
                     say `emma goal {goal}`."
                ));
            }
        }
    }

    let command = match command {
        Some(Command::Run(_)) | None => Command::Run(joined),
        Some(Command::Api(_)) => Command::Api(joined),
        // One word, like `set-model`, which this is an alias for. Without the
        // check `emma model the API surface` stored a model id of "the API
        // surface" — and the interactive prompt disagreed with it, treating the
        // same four words as a goal (`typed_at_the_prompt` accepts `model` only
        // at one or two words). One hazard, two doors, and they answered
        // differently.
        Some(Command::Model(_)) => Command::Model(match joined {
            Some(text) if text.contains(' ') => {
                return Err(format!(
                    "`model` takes a model id, one word; got `{text}`. If that was the goal, \
                     say `emma goal {text}`."
                ))
            }
            other => other,
        }),
        // One word, required. A provider name with a space in it is a typo, and
        // guessing which half was meant is how `emma set-provider anthropic
        // please` ends up storing a key for a provider called `anthropic
        // please`.
        Some(Command::SetProvider { .. }) => Command::SetProvider {
            name: one_word("set-provider", "a provider name", joined)?,
            key: opts.key.clone(),
        },
        Some(Command::SetModel(_)) => {
            Command::SetModel(one_word("set-model", "a model id", joined)?)
        }
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
        // **The same rule as `init`, and it was missing here.** `emma agents are
        // slow` became `Command::Agents` with "are slow" dropped on the floor,
        // and `config check --whatever` the same. `init`'s own comment one arm
        // above says why that is wrong — "silently discarding it is the wrong
        // half of the guess" — and these two took the wrong half.
        //
        // A user who typed extra words meant something by them. Either they
        // wanted a goal, or they expected the command to take an argument it
        // does not. Both are answered by saying so.
        Some(cmd @ (Command::Agents | Command::ConfigCheck)) => match joined {
            Some(extra) => {
                let name = match cmd {
                    Command::Agents => "agents",
                    _ => "config check",
                };
                return Err(format!(
                    "`{name}` takes no arguments; got `{extra}`. If that was the goal, say \
                     `emma goal {extra}`."
                ));
            }
            None => cmd,
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
        (Some("-h" | "--help"), 1) => return Typed::Answer(help()),
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
        // `set-provider anthropic --key sk-…` is five words, and the argument
        // count is therefore not what keeps a goal safe here — the hyphenated
        // name is. No English sentence starts with `set-provider`.
        (Some("set-provider"), _) => "set-provider",
        (Some("set-model"), _) => "set-model",
        (Some("config"), 2) if words[1].eq_ignore_ascii_case("check") => "config check",
        (Some("agents"), 1) => "agents",
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

/// The single positional a `set-*` command takes, or a refusal naming what was
/// wanted. Nothing here is echoed for `--key`, which never reaches this.
fn one_word(command: &str, wanted: &str, joined: Option<String>) -> Result<String, String> {
    match joined {
        Some(text) if !text.contains(' ') => Ok(text),
        Some(text) => Err(format!(
            "`{command}` takes {wanted}, one word; got `{text}`."
        )),
        None => Err(format!("`{command}` needs {wanted}. See `emma --help`.")),
    }
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
    fn set_provider_takes_a_name_and_optionally_a_key_and_a_model() {
        let cli = p(&["set-provider", "anthropic"]).unwrap();
        assert_eq!(
            cli.command,
            Command::SetProvider {
                name: "anthropic".into(),
                key: None
            }
        );
        let cli = p(&[
            "set-provider",
            "anthropic",
            "--key",
            "sk-ant-x",
            "--model",
            "claude-x",
            "--no-verify",
        ])
        .unwrap();
        assert_eq!(
            cli.command,
            Command::SetProvider {
                name: "anthropic".into(),
                key: Some("sk-ant-x".into())
            }
        );
        assert_eq!(cli.opts.model.as_deref(), Some("claude-x"));
        // A name is required, and a sentence is not a name.
        assert!(p(&["set-provider"]).unwrap_err().contains("provider name"));
        assert!(p(&["set-provider", "anthropic", "please"]).is_err());
    }

    #[test]
    fn set_model_takes_an_id_and_an_optional_provider() {
        assert_eq!(
            p(&["set-model", "claude-x"]).unwrap().command,
            Command::SetModel("claude-x".into())
        );
        let cli = p(&[
            "set-model",
            "claude-x",
            "--provider",
            "anthropic",
            "--force",
        ])
        .unwrap();
        assert_eq!(cli.opts.provider.as_deref(), Some("anthropic"));
        // Bare `set-model` lists, in a build that can list. This one cannot, so
        // it says what it needs rather than doing something else.
        assert!(p(&["set-model"]).unwrap_err().contains("model id"));
        // `--provider` is also a one-run override for an ordinary goal.
        let cli = p(&["--provider", "anthropic", "goal", "do it"]).unwrap();
        assert_eq!(cli.opts.provider.as_deref(), Some("anthropic"));
        assert_eq!(cli.command, Command::Run(Some("do it".into())));
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
            "set-provider anthropic",
            "set-model claude-x",
            "emma set-provider anthropic --key sk-ant-not-a-real-key",
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

    /// A mistyped subcommand is refused instead of becoming a paid model call.
    ///
    /// **Found by running `emma nonsense-subcommand` expecting a refusal.** It
    /// launched a real session instead — a shell, a `Glob *` that hit the
    /// 1000-path cap, several greps — all to answer a word nobody meant as a
    /// question. Every non-flag word falls through to a goal, so `emma agent`,
    /// one letter short of `agents`, spends money rather than erroring.
    ///
    /// The interactive prompt has been guarded against the neighbouring mistake
    /// since a run that cost 580,000 tokens on a misread `init`. The argv path
    /// had no equivalent: one hazard, one door closed and one open.
    #[test]
    fn a_single_word_near_miss_of_a_command_is_refused_not_charged_for() {
        for (typo, meant) in [
            ("agent", "agents"),
            ("ini", "init"),
            ("mode", "model"),
            ("confg", "config"),
            ("set-mode", "set-model"),
            ("Agents", "agents"),
            // **Transpositions, the commonest typing error and the class this
            // guard shipped without.** Levenshtein counts an adjacent swap as
            // TWO edits, so a doc saying "one edit" quietly excluded them and a
            // reviewer certified four of these starting real sessions on the
            // release binary. `inti` is the transposition of the exact word
            // whose misread cost the 580,000-token run this guard exists for.
            ("inti", "init"),
            ("modle", "model"),
            ("agnets", "agents"),
            ("confgi", "config"),
        ] {
            let e = p(&[typo]).expect_err("a near miss was accepted as a goal");
            assert!(
                e.contains(meant),
                "the refusal does not name what was meant: {e}"
            );
            assert!(
                e.contains("emma goal"),
                "the refusal does not offer the escape hatch: {e}"
            );
        }
    }

    /// A goal that merely *rhymes* with a command still runs.
    ///
    /// The positive control, and it is the one that keeps this guard usable: a
    /// refusal that fires too widely makes a legitimate one-word goal
    /// impossible to type, and the transposition arm above widened the net. The
    /// test is `is_transposition`'s narrowness — `abc` vs `cba` is two swaps and
    /// must not match, or every three-letter goal becomes unreachable.
    #[test]
    fn a_word_that_is_not_a_near_miss_is_still_a_goal() {
        for word in ["deploy", "tinker", "cba", "tsting", "refactor"] {
            let parsed = p(&[word]);
            assert!(
                parsed.is_ok(),
                "`{word}` is not within one edit of any command and was refused anyway: {:?}",
                parsed.err()
            );
        }
    }

    /// Extra words after an argument-less command are refused, not dropped.
    ///
    /// `init` already refused them, with the reason in its own comment:
    /// "silently discarding it is the wrong half of the guess". `agents` and
    /// `config check`, one arm below it, took exactly that half — `emma agents
    /// are slow` ran the ledger and dropped "are slow" on the floor. A user who
    /// typed extra words meant something by them.
    #[test]
    fn extra_words_after_an_argument_less_command_are_refused() {
        for (args, name) in [
            (vec!["agents", "are", "slow"], "agents"),
            (vec!["config", "check", "everything"], "config check"),
            (vec!["init", "the", "database"], "init"),
        ] {
            let e = p(&args).expect_err("extra words were silently discarded");
            assert!(
                e.contains(name),
                "the refusal does not name the command: {e}"
            );
        }
    }

    /// And the guard stays narrow: a real goal is still a goal.
    ///
    /// A sentence is never one edit from a subcommand, and a single word that is
    /// genuinely wanted has `emma goal <word>`. Widening this to two edits would
    /// start refusing goals, which is the failure this must not trade for.
    #[test]
    fn a_real_goal_is_not_mistaken_for_a_typo() {
        for goal in [
            "fix the build",
            // `init the database` and `model the API surface` are deliberately
            // absent: both are subcommands in first position on argv, and both
            // now refuse their extra words loudly rather than storing or
            // ignoring them. The interactive prompt reads either as a goal,
            // which is an asymmetry the refusals make visible instead of silent.
            // `model the API surface` is deliberately absent: on argv `model`
            // is a subcommand in first position, and it now refuses a
            // multi-word argument rather than storing one. The prompt reads the
            // same words as a goal, which is the asymmetry that refusal makes
            // visible instead of silent.
            "refactor",
            "test",
        ] {
            let cli = p(&goal.split(' ').collect::<Vec<_>>()).expect(goal);
            assert!(
                matches!(cli.command, Command::Run(Some(_))),
                "`{goal}` stopped being a goal"
            );
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
        let help = help();
        let session = session_help();
        assert!(help.contains("/exit"), "the help does not say how to leave");
        assert!(help.contains("/quit"));
        assert!(help.contains("Ctrl-C"));
        // …and it is the *same bytes* as `/help` prints, rather than a second
        // text that agrees today. The exit lines live in the session half, so
        // this also pins that `--help` still carries it.
        assert!(help.contains(&session), "help and session_help diverged");
        assert!(session.contains("/exit"));
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
