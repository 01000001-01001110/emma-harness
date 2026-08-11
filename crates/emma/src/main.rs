//! Wiring, and nothing else. Every decision this binary makes lives in the
//! library beside it, where a scripted `Provider` can drive it without a
//! network, a terminal or a signal handler.

use std::sync::Arc;

use anyhow::{Context, Result};
use emma::agent::{Agent, Ending, Interrupt, Setup};
use emma::approval::{Approvals, Asker, Gate};
use emma::cli::{self, Command};
use emma::goal::{Goal, MarkerClaim};
use emma::session::{self, SessionLog};
use emma::settings;
use emma::skill::Skill;
use emma::term::{Term, Welcome};
use emma_harness::Harness;
use emma_llm::{auth, AnthropicProvider, Mode, Provider};
use emma_tool_api::{Registry, Tool};

fn main() -> Result<()> {
    let cli = match cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    // These five touch a file and a terminal and nothing else. Running them
    // without a runtime means a broken runtime can never be the thing that
    // stops somebody fixing their credentials — and `init` belongs here for the
    // sharper version of the same reason: the command that fixes "Emma will not
    // start in this directory" must not need Emma to start.
    match cli.command {
        Command::Help => {
            print!("{}", cli::HELP);
            return Ok(());
        }
        Command::Version => {
            println!("emma {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Command::Api(key) => return emma::commands::api(key),
        Command::Model(name) => return emma::commands::model(name),
        Command::Init => {
            let cwd = std::env::current_dir().context("reading the working directory")?;
            return emma::commands::init(&cwd, &mut std::io::stdout());
        }
        _ => {}
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(run(cli))
}

async fn run(cli: cli::Cli) -> Result<()> {
    let cwd = std::env::current_dir().context("reading the working directory")?;
    // Boot before anything else that can fail: a harness that will not load is
    // the one error a user must see before any output suggests work started.
    let harness = Arc::new(Harness::boot()?);

    let mut registry = Registry::new();
    // The tracker is dropped on purpose. `fs_tools` hands it back for callers
    // that want to inspect or reset it, which a binary does not; Read, Write and
    // Edit already hold their own clones of the one shared instance, so the
    // read-before-write rule survives this binding going out of scope.
    let (fs, _tracker) = emma_tools_fs::fs_tools();
    for tool in fs {
        registry.register(tool);
    }
    // The task surface. Registered unconditionally: the file it maintains is
    // created on first write, so there is no configuration to be absent, and a
    // goal-holding loop with no way to write down what is left is the thing the
    // owner asked for and did not have. Two of these four write, and are
    // exempted from the approval prompt by name — see `approval::EXEMPT`, which
    // carries the argument.
    for tool in emma_tools_tasks::task_tools() {
        registry.register(tool);
    }
    // Code intelligence: `FindReferences`, `GoToDefinition`, `Hover`,
    // `DocumentSymbols`. All four are `read_only` and prompt nobody. The pool is
    // dropped on purpose, as `_tracker` above is — the four tools hold clones of
    // the one shared instance, so the language server outlives this binding and
    // dies with them at exit.
    let (lsp, _lsp_pool) = emma_tools_lsp::lsp_tools();
    for tool in lsp {
        registry.register(tool);
    }
    // Registered only when the harness resolved skills — an unusable capability
    // with a description attached is a trap.
    if let Some(skill) = Skill::new(harness.clone()) {
        registry.register(Arc::new(skill) as Arc<dyn Tool>);
    }
    // The web surface, under the same rule and for the same reason. `web_tools`
    // resolves Chrome and the Brave key *first* and returns only what can
    // actually work: tustle-agent shipped a `web_search` that registered with no
    // key and failed on every call, and the model had no way to learn the tool
    // was decoration. What it left out is kept and reported below rather than
    // dropped, because a capability that silently is not there is the same trap
    // one step quieter.
    //
    // Both of these declare `reaches_network: true`, which is what makes
    // registering them a decision rather than a default: the approval gate asks
    // a human for each new host, once per session. See `approval.rs`.
    let web = emma_tools_web::web_tools();
    for tool in web.tools {
        registry.register(tool);
    }
    // Consumes the registry: the unfiltered one must not survive the call.
    let tools = harness.select_tools(registry)?;

    if cli.command == Command::ConfigCheck {
        return emma::commands::config_check(&harness, &tools, &cwd, &web.skipped);
    }

    let opts = cli.opts;
    let term = if opts.print {
        Term::printing()
    } else {
        Term::interactive()
    };

    // Before the reader, because the reader may *be* the delivery. A viewport
    // means raw mode, and raw mode is the state in which the terminal stops
    // turning Ctrl-C into a signal — so the keystroke has to reach this flag
    // directly. `install` wires the signal as well; a fallback run has no other
    // way to be interrupted, and two paths tripping one flag costs nothing.
    let interrupt = Interrupt::new();
    interrupt.install();

    let (gate, asker) = match (opts.skip_permissions, opts.print) {
        (true, _) => (Gate::SkipAll, Asker::Scripted(Default::default())),
        (false, true) => (Gate::Unattended, Asker::Scripted(Default::default())),
        (false, false) => {
            let signal = interrupt.clone();
            // The `/` menu's vocabulary is this run's, taken from the harness
            // that actually loaded. Nothing else may put a name in it.
            let menu = emma::term::menu::Menu::for_project(&harness.command_names());
            (
                Gate::Ask,
                Asker::Terminal(term.line_source(menu, move || signal.trip()).into()),
            )
        }
    };
    // After the terminal exists, because this is the first thing a user needs
    // when a page read they expected does not happen: the tool is absent, and
    // here is the sentence saying which one and why.
    for line in &web.skipped {
        term.note(line);
    }
    if gate == Gate::SkipAll {
        // Loud, every run, before anything happens. A bypass nobody is
        // reminded of is a bypass somebody left on.
        term.banner(
            "APPROVAL IS OFF. Every write, edit and shell command runs without asking, in \
             this directory.",
        );
    }
    let approvals = Approvals::new(gate, asker);

    let home = auth::home_dir();
    let (model, _source) = settings::resolve(opts.model.as_deref(), home.as_deref());
    let key = auth::load_default()?;
    let provider = AnthropicProvider::new(key, Some(model));

    let session_dir = opts
        .session_dir
        .clone()
        .or_else(|| SessionLog::default_dir(home.as_deref()));

    // Resolved before the log is opened, because a resumed run continues the
    // same file rather than starting a new one: one goal's transcript in one
    // place, and a resume of a resume that folds the whole thing rather than
    // the last leg of it.
    let restored = match &cli.command {
        Command::Resume { session, .. } => {
            let dir = session_dir.clone().context(
                "no session directory: the home directory could not be determined. Name one \
                 with --session-dir.",
            )?;
            let path = session::locate(&dir, session.as_deref(), &cwd)?;
            let restored = session::restore(&path)?;
            // The spend is shown against the caps rather than on its own,
            // because the number that matters to somebody deciding whether to
            // resume is the headroom: a session restored at 59/60 calls will
            // stop again immediately, and that is worth knowing before it does
            // rather than after.
            let b = &opts.budgets;
            let r = &restored.resumed;
            term.note(&format!(
                "resuming {} — {} messages restored. Already spent on this goal: {}/{} calls, \
                 {}/{} tokens, {}/{} nudges.",
                restored.id,
                r.messages.len(),
                r.iterations,
                b.max_iterations,
                r.tokens,
                b.max_tokens,
                r.kicks,
                b.max_kicks
            ));
            // Warned about, never refused: the user asked to resume, and what
            // is dangerous is not the change but the change being invisible.
            for line in restored.continuity.differences(
                &harness.instructions_hash(),
                &tools.schema_hash(),
                provider.model_id(),
            ) {
                term.warn(&line);
            }
            Some(restored)
        }
        _ => None,
    };

    // Kept because `session_dir` is moved into the log below and the first-run
    // check needs the same answer: "is there anywhere sessions are recorded, and
    // has anything been recorded here".
    let session_dir_for_welcome = session_dir.clone();

    let session_id = match &restored {
        Some(restored) => restored.id.clone(),
        None => SessionLog::new_id(),
    };
    let log = match session_dir {
        Some(dir) => match SessionLog::open(&dir, &session_id) {
            Ok(log) => log,
            Err(e) => {
                // A transcript that cannot be written is worth one warning, not
                // a refusal to work.
                term.warn(&format!("no session transcript: {e}"));
                SessionLog::none()
            }
        },
        None => SessionLog::none(),
    };
    if !opts.print {
        // The run's identity, once, as an ordinary line that scrolls away like
        // any other. Model, directory and transcript path only: they are fixed
        // for the process, which is the entire reason they can be stated once
        // and left. Spend changes every turn and is reported after each goal.
        //
        // There is no pinned status row and this is not one pretending. A fixed
        // row costs a scroll region, a scroll region costs scrollback, and the
        // owner's first complaint was that he could not scroll.
        term.set_status(provider.model_id(), &cwd, log.path());
        // What the interactive session understands, said once, because none of
        // it is guessable. `/exit` and `/quit` have always worked and were
        // documented nowhere; the harness's own commands are whatever the user
        // put in `.emma/commands/`, so they are listed rather than described.
        //
        // Skipped when the input box is drawn, because the hint under it says
        // the same thing and keeps saying it — a line that scrolls away is the
        // fallback path's copy of a fact the box carries permanently.
        if !term.framed() {
            term.note("/exit or /quit ends the session · Ctrl-C interrupts a running goal");
        }
        let commands = harness.command_names();
        if !commands.is_empty() {
            term.note(&format!(
                "commands (from commands/ in this project's harness): {}",
                commands
                    .iter()
                    .map(|c| format!("/{c}"))
                    .collect::<Vec<_>>()
                    .join("  ")
            ));
        }
        // Once, for somebody who has not been here before. Detected from the
        // transcripts rather than from a marker file — see `session::first_run`,
        // which also supplies the sentence saying how it decided, because "why
        // am I seeing this?" is a returning user's first question.
        //
        // Composed from the run that is actually starting: the harness that
        // loaded, the tools that survived selection, this project's commands and
        // skills. A welcome that lists capabilities the run does not have
        // teaches, first thing, that Emma's account of itself cannot be trusted.
        if let Some(why) = session::first_run(session_dir_for_welcome.as_deref(), &cwd) {
            term.welcome(&Welcome {
                reason: why.reason().to_string(),
                harness: format!(
                    "{} {} {}",
                    match harness.flavor {
                        emma_harness::Flavor::Claude => "claude",
                        emma_harness::Flavor::Emma => "emma",
                    },
                    "at",
                    harness.root.display()
                ),
                tools: tools.names().iter().map(|n| n.to_string()).collect(),
                commands: commands.iter().map(|c| c.to_string()).collect(),
                skills: harness
                    .skill_names()
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            });
        }
    }

    let budgets = opts.budgets;
    let agent = Agent::new(Setup {
        provider: &provider,
        harness: &harness,
        tools: &tools,
        approvals: &approvals,
        log: &log,
        term: &term,
        interrupt: interrupt.clone(),
        done: &MarkerClaim,
        cwd: cwd.clone(),
        session_id,
        budgets,
        caching: opts.caching,
        mode: if opts.print {
            Mode::Batch
        } else {
            Mode::Stream
        },
    });

    // The restored conversation and counters go in here, and the loop below is
    // unchanged by the resume: a resumed run is a run with something behind it,
    // not a different mode.
    let mut agent = match restored {
        Some(restored) => agent.resuming(restored.resumed),
        None => agent,
    };

    let seed = match cli.command {
        Command::Run(seed) => seed,
        Command::Resume { goal, .. } => goal,
        _ => unreachable!("every other command returned above"),
    };

    let mut next = seed;
    let mut last = Ending::Done;
    loop {
        // Whether this line came from a person at the prompt or from the
        // command line. Only the first is second-guessed: `emma goal "init"` is
        // somebody stating a goal in the one place goals are unambiguous, and
        // intercepting it would be overruling them.
        let mut from_the_prompt = false;
        let raw = match next.take() {
            Some(text) => text,
            None => {
                from_the_prompt = true;
                term.goal_prompt();
                // `read_line` drains before it waits — a line typed before this
                // prompt existed cannot answer it — and the terminal echoes the
                // return itself, which `prompt_answered` is what tells the
                // input box about. Both happen before the line is looked at.
                let line = approvals.read_line().await;
                term.prompt_answered(line.as_deref());
                match line {
                    Some(line) if line.trim().is_empty() => continue,
                    Some(line) => line,
                    None => break,
                }
            }
        };
        if matches!(raw.trim(), "/exit" | "/quit") {
            break;
        }
        // One of Emma's own command lines, typed where goals go. Answered for
        // free rather than handed to the model, which previously went and found
        // out what `init` does the expensive way. See `cli::typed_at_the_prompt`
        // for why the match is as narrow as it is.
        if from_the_prompt {
            match cli::typed_at_the_prompt(&raw) {
                cli::Typed::Goal => {}
                cli::Typed::Elsewhere(say) | cli::Typed::Answer(say) => {
                    for line in say.lines() {
                        term.note(line);
                    }
                    continue;
                }
            }
        }
        // Expanded at intake, so the model never learns commands exist.
        let text = match harness.expand_command(raw.trim()) {
            Some(expansion) => {
                term.note(&format!("/{} expanded", expansion.command));
                expansion.text
            }
            None => raw,
        };

        let outcome = agent.run_goal(&Goal::new(text)).await;
        // "cache-weighted" rather than "tokens", because it is not the number
        // the provider reports and a person comparing this line with a bill
        // should know which one it is: a cached read counts here at the tenth
        // of a token it costs. The raw provider counts are in the transcript.
        term.ending(
            &outcome.ending.message(&budgets),
            matches!(outcome.ending, Ending::Done | Ending::Answered),
            outcome.iterations,
            outcome.tokens,
        );
        last = outcome.ending;
        if opts.print || interrupt.tripped() {
            break;
        }
    }

    // A goal that did not finish must not look like one that did to whatever
    // ran `emma -p` in a script.
    // `Answered` counts with `Done`. A question put to `emma -p` and answered
    // is the script getting what it asked for; exiting non-zero on it would
    // make every `emma -p "what does this do?"` look like a failed run.
    if opts.print && !matches!(last, Ending::Done | Ending::Answered) {
        // `process::exit` runs no destructors, so `Term`'s would not run here.
        // `-p` never draws a frame and this call is therefore a no-op today —
        // it is here because "the exit path that skips Drop" is exactly the
        // shape that leaves somebody's shell with a scroll region set, and the
        // next person to make this branch reachable interactively should not
        // have to notice that.
        emma::term::restore_terminal();
        std::process::exit(1);
    }
    Ok(())
}
