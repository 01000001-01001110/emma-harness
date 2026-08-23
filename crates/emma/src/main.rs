//! Wiring, and nothing else. Every decision this binary makes lives in the
//! library beside it, where a scripted `Provider` can drive it without a
//! network, a terminal or a signal handler.

use std::io::IsTerminal;
use std::sync::Arc;

use anyhow::{Context, Result};
use emma::agent::{Agent, Ending, Interrupt, Setup, Spend};
use emma::approval::{Approvals, Asker, Gate};
use emma::cli::{self, Command};
use emma::goal::{Goal, MarkerClaim};
use emma::session::{self, SessionLog};
use emma::settings;
use emma::skill::Skill;
use emma::term::{Term, Welcome};
use emma_harness::Harness;
use emma_llm::{auth, Mode, Provider};
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
        // The model, when one was named, is `--model` — the same flag that
        // overrides a run. See `cli::Command::SetProvider`.
        Command::SetProvider { name, key } => {
            return emma::commands::set_provider(&name, key, cli.opts.model)
        }
        Command::SetModel(model) => {
            return emma::commands::set_model(&model, cli.opts.provider.as_deref())
        }
        Command::Init => {
            let cwd = std::env::current_dir().context("reading the working directory")?;
            return emma::commands::init(&cwd, &mut std::io::stdout());
        }
        // Reads the transcripts and prints. No harness, no key, no model — so it
        // belongs here with the others rather than inside the runtime, and it
        // still answers when the thing being investigated is why a run will not
        // start.
        Command::Agents => {
            let dir = cli
                .opts
                .session_dir
                .clone()
                .or_else(|| SessionLog::default_dir(auth::home_dir().as_deref()));
            return emma::commands::agents(dir.as_deref(), &mut std::io::stdout());
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
    //
    // It also carries the five browser session tools, which are the same
    // decision one size larger: inside a session the model acts as the user, on
    // a page that may hold their login. `BrowserAct` and `BrowserFill` declare
    // `read_only: false` so the gate asks about each call, and acting is
    // additionally confined to the domains in the user's own
    // `~/.emma/browser-allowlist.json`.
    let web = emma_tools_web::web_tools();
    for tool in web.tools {
        registry.register(tool);
    }
    // **Held for the length of the run, deliberately.** This is the handle on
    // every live Chrome, and dropping it kills them — which is what should
    // happen when this function returns and must not happen before. `tools/lsp`
    // records the bug this shape is arranged against: "`tools/web` leaked a
    // Chrome per session because its teardown ran in `main`". Teardown is in the
    // pool's `Drop` now; `main`'s only job is to keep the pool alive until the
    // loop is finished with it, which the binding below is.
    let browser_pool = web.browser.clone();
    if cli.command == Command::ConfigCheck {
        // The one path that does not delegate: it needs no key, and `Delegate`
        // cannot be built without a provider. It prints the agent catalogue from
        // the harness instead, and says that is what it is doing.
        let tools = harness.select_tools(registry)?;
        // `live: None` and stdout: this path has no running session, so it
        // prints exactly the bytes it always did. The session door passes a
        // `Live` and a buffer — see `session_command::run`.
        return emma::commands::config_check(
            &harness,
            &tools,
            &cwd,
            &web.skipped,
            None,
            &mut std::io::stdout(),
        );
    }

    let opts = cli.opts;
    // `Arc` because a delegation borrows this same terminal — `Term::subordinate`
    // shares the frame and the stdin path so a subagent's approval prompt reaches
    // the same keyboard, while the status meters stay the parent's.
    // The theme, resolved once and here. It is read before the terminal exists
    // because the palette is part of building one, and it is never read again:
    // `/theme` writes the selection and says outright that the next start is
    // what shows it, so there is no moment at which this value and the screen
    // disagree. A broken theme costs colour and nothing else — `load` never
    // fails — and the sentences it produces go through `Term::warn` below,
    // once the thing that can say them exists.
    // `None` is "whatever settings.json names": there is no `--theme` flag, and
    // `theme::load` keeps the parameter for the one-run override that would be
    // one if it were ever wanted.
    let (theme, theme_notices) =
        emma::term::theme::load(auth::home_dir().as_deref(), Some(&harness.root), None);
    let term = Arc::new(if opts.print {
        Term::printing(theme)
    } else {
        Term::interactive(theme)
    });

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
    // Same rule, same place: a theme that could not be read entirely, or at
    // all, has already been fallen back from — saying so is the difference
    // between "Emma ignored my file" and "Emma found the typo and told me".
    // Through `warn`, which is the side channel, so `-p`'s stdout is untouched.
    for line in &theme_notices {
        term.warn(line);
    }
    if gate == Gate::SkipAll {
        // Loud, every run, before anything happens. A bypass nobody is
        // reminded of is a bypass somebody left on.
        term.banner(
            "APPROVAL IS OFF. Every write, edit and shell command runs without asking, in \
             this directory.",
        );
    }
    let home = auth::home_dir();

    // The permission rules, from two scopes, merged rather than layered: they
    // compose safely because `deny` beats `allow` at match time whichever file
    // each came from. Project scope carries all three lists; the user's
    // `~/.claude/settings.json` contributes `deny` only, and
    // `emma_harness::user_permissions` carries the whole argument for that
    // asymmetry — it is the same one `discover_in` makes about not adopting
    // another program's global configuration.
    //
    // Never fatal. A user settings file that cannot be read is skipped there; a
    // project one that cannot be *parsed* already failed the boot in `Harness`,
    // and a rule this build cannot evaluate becomes a note printed below.
    let mut entries = harness.permissions().to_vec();
    let (user_entries, user_notes) = emma_harness::user_permissions(home.as_deref())?;
    entries.extend(user_entries);
    // A deny list that could not be read is a protection the operator believes
    // they have. Loud, and before the rule notes below, because it explains why
    // rules they wrote are missing from that list entirely.
    for note in &user_notes {
        term.warn(note);
    }
    let (rules, rule_notes) = emma::permissions::Rules::parse(&entries);
    for note in &rule_notes {
        // Warnings rather than notes: every one of these is a line somebody
        // wrote in a settings file that is not doing what they think it is, and
        // the `deny` ones are a protection they believe they have.
        term.warn(note);
    }

    // Shared with every nested run, and the sharing is the design: one gate, one
    // set of session grants, one rule set, one keyboard. A subagent with its own
    // `Approvals` would re-ask for a host the user already approved — and could
    // be handed a bypass the parent was not.
    let approvals = Arc::new(
        // The one place the process is asked. The decision itself is
        // `Approvals::for_stdin`, in the library, where it has a test -- this
        // used to be the `if` and deleting it brought the reported defect
        // straight back with only a compiler warning to show for it.
        Approvals::new(gate, asker)
            .for_stdin(std::io::stdin().is_terminal())
            .with_rules(rules, Some(emma::permissions::file_for(&harness.root))),
    );
    // The one place a running provider is chosen. An unknown name fails here
    // rather than falling back, so a mis-set provider cannot look like a
    // working one — see `settings::resolve_kind`.
    let (kind, resolved) = settings::resolve_kind(
        opts.provider.as_deref(),
        opts.model.as_deref(),
        home.as_deref(),
    )?;
    let key = auth::load_default(kind)?;
    let provider: Arc<dyn Provider> = kind.build(key.clone(), Some(resolved.model.clone()));
    // The one cell everything that resolves a provider *late* reads — today
    // that is `Delegate` and nothing else. Written only by `/model`. See
    // `agent::Running`.
    let running = emma::agent::Running::new(provider.clone());

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
            // Reported before the resume note, because it changes what that
            // note means: bare `--resume` claims to be continuing the session
            // you were last running here, and a newer file that would not read
            // makes that quietly untrue. See `session::skipped_sessions_note`.
            let (path, skipped) = session::locate_reporting(&dir, session.as_deref(), &cwd)?;
            if let Some(note) = session::skipped_sessions_note(&skipped) {
                term.warn(&note);
            }
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
            // **Damage is said out loud, and until 2026-08-23 it was not.**
            // `lost_records` and `damage` were both carried on the value with
            // docs saying a caller should act on them, and this caller read
            // neither. A reviewer proved it by deleting each field and building
            // clean. The note itself lives in `session` so it can be tested;
            // what belongs here is only the decision to show it.
            if let Some(note) = session::resume_damage_note(&restored) {
                term.warn(&note);
            }
            // The continuity warnings are emitted further down, once the final
            // tool surface exists: `Delegate` is registered after this point, so
            // a schema hash taken here would be a hash of a surface no request
            // will carry.
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
    let log = Arc::new(match session_dir {
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
    });

    // The token meter this process spends against, and the caps its status line
    // measures against. Both are set here rather than in `Agent::new`, because
    // both are facts about the *process*: a nested `Agent` constructed mid-run
    // would otherwise re-point the meters at a sub-run's caps for the rest of it.
    let budgets = opts.budgets;
    let spend = Spend::new();
    term.set_budgets(budgets.max_context, budgets.max_tokens);

    // Delegation, when this harness resolved any agent types.
    //
    // Registered *before* `select_tools`, so a persona's `tools` list decides
    // whether this run may delegate at all — leave `Delegate` out of it and the
    // capability is gone for real rather than by convention. The child registries
    // are built from `available`, which cannot contain `Delegate` because
    // `Delegate` does not exist yet: that is the whole of the recursion guard,
    // and it is structural rather than a check somebody has to remember.
    let available: Vec<Arc<dyn Tool>> = registry.iter().cloned().collect();
    let (delegate, agent_notes) = emma::Delegate::new(
        emma::Nest {
            harness: harness.clone(),
            approvals: approvals.clone(),
            log: log.clone(),
            term: term.clone(),
            interrupt: interrupt.clone(),
            spend: spend.clone(),
            cwd: cwd.clone(),
            session_id: session_id.clone(),
            caching: opts.caching,
            budgets,
            running: running.clone(),
        },
        harness.agent_types(),
        &available,
        harness.tools(),
        // A per-type model override, honoured rather than ignored: an agent file
        // saying `model: claude-sonnet-4-5` that quietly runs on something else
        // is a lie the user cannot see. Same provider and same key. An agent
        // file naming *another provider's* model is out of scope: `def.model` is
        // a bare id in this provider's namespace.
        //
        // **`None` means "the parent's, at delegation time".** This used to
        // compare the named id against the model resolved at boot and hand back
        // the parent's own client when they matched — an optimisation that
        // became a bug the moment `/model` existed, because the captured id and
        // the captured client both go stale on the first change. Building a
        // separate client for a file that names one costs nothing (same key,
        // same id) and cannot be stale; leaving the unnamed case unresolved is
        // what makes `/model` reach subagents at all.
        &|wanted| wanted.map(|named| kind.build(key.clone(), Some(named.to_string()))),
    );
    for note in harness.agent_notes() {
        term.note(note);
    }
    // **The other half of the same pattern, and it was missing.** `HARD-001`
    // was reopened once because the skipped-skill count went to stderr, and its
    // own text says "the shape to copy was one file away — `load_agents`
    // already returned notes and `Harness` already exposed `agent_notes`". The
    // shape was copied halfway: `skill_notes` was added, a test asserted it, and
    // nothing in production ever read it. A reviewer proved it by deleting the
    // one remaining `eprintln!` and watching the whole harness suite stay green.
    for note in harness.skill_notes() {
        term.note(note);
    }
    for note in harness.command_notes() {
        term.note(note);
    }
    for note in &agent_notes {
        term.note(note);
    }
    if let Some(delegate) = delegate {
        registry.register(Arc::new(delegate) as Arc<dyn Tool>);
    }
    // Consumes the registry: the unfiltered one must not survive the call.
    let tools = harness.select_tools(registry)?;

    // Two mistakes that cannot be seen until the tool surface exists, which is
    // why they are checked here rather than beside the other rule notes: a rule
    // whose tool name differs from a real one only by case, and a `domain:`
    // specifier on a tool that never reaches the network. Both parse, both are
    // kept, and both match nothing — and in a deny list that is a protection
    // the operator believes they have.
    let known = tools.names();
    let reaching: Vec<&'static str> = tools
        .iter()
        .filter(|t| t.meta().reaches_network)
        .map(|t| t.name())
        .collect();
    for note in emma::permissions::Rules::unmatchable_here(&entries, &known, &reaching) {
        term.warn(&note);
    }

    if let Some(restored) = &restored {
        for line in restored.continuity.differences(
            &harness.instructions_hash(),
            &tools.schema_hash(),
            provider.model_id(),
            &std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ) {
            term.warn(&line);
        }
    }
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
        // A `statusLine` in the harness's settings replaces the line above for
        // the rest of the run. Resolution, containment and the timeout are the
        // harness's; `set_status_source` is the whole of the wiring. A note
        // rather than a failure when configuration asked for one and could not
        // have it — the bottom row is decoration, and `statusline.rs` argues the
        // ruling.
        if let Some(note) = harness.status_line_note() {
            term.warn(note);
        }
        if let Some(line) = harness.status_line() {
            term.set_status_source(std::sync::Arc::new(line.clone()));
        }
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

    // Declared here rather than inside `Setup` so the exit path below can still
    // reach it after the agent is gone: a session that ends leaving children
    // running is a leak, and one that kills them silently is a surprise.
    let background = emma_tool_api::background::Registry::new();

    let agent = Agent::new(Setup {
        // One registry for the whole interactive session, so a task started in
        // one goal is still findable in the next. Scoping it per goal would make
        // "run the build in the background, then ask me about it" impossible,
        // which is the case background execution exists for.
        background: background.clone(),
        provider: provider.clone(),
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals: &approvals,
        log: &log,
        term: &term,
        interrupt: interrupt.clone(),
        spend: spend.clone(),
        done: &MarkerClaim,
        cwd: cwd.clone(),
        session_id: session_id.clone(),
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
    // The provider binding travels into the command dispatcher, which is the
    // only thing allowed to change it. `main` and the agent cannot disagree
    // about the running model, because there is one binding.
    let mut provider = provider;
    // What the screen is drawn in, snapshotted before anything can change it.
    // A theme is read once, when the terminal is built, so this stays true for
    // the life of the process — where `settings.json` does not, because
    // `/theme` writes that same key and the two are then deliberately
    // different facts.
    let theme_at_start = home.as_deref().and_then(|h| emma::settings::load(h).theme);
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
                let line = approvals.read_line(&term).await;
                term.prompt_answered(line.as_deref());
                match line {
                    Some(line) if line.trim().is_empty() => continue,
                    Some(line) => line,
                    None => break,
                }
            }
        };
        // One of Emma's own commands, typed here or picked from the `/` menu —
        // which sends the same string down the same channel, so there is one
        // path rather than two. `/exit` and `/quit` are ordinary members of this
        // set now; they used to be a `matches!` bolted in front of everything
        // else, which made the way out of the session the one command whose
        // dispatch nothing tested.
        //
        // Ahead of `expand_command`, so a built-in beats a project command of
        // the same name. That has always been true of `/exit`; `config check`
        // and the menu now say so out loud.
        if let Some(cmd) = emma::session_command::parse(&raw) {
            let mut session = emma::session_command::Session {
                agent: &mut agent,
                term: &term,
                approvals: &approvals,
                harness: &harness,
                tools: &tools,
                cwd: &cwd,
                session_dir: session_dir_for_welcome.as_deref(),
                unavailable: &web.skipped,
                provider: &mut provider,
                running: &running,
                kind,
                key: key.clone(),
                log_path: log.path().to_path_buf(),
                home: home.clone(),
                theme_at_start: theme_at_start.clone(),
            };
            match emma::session_command::run(cmd, &mut session).await {
                emma::session_command::Flow::Exit => break,
                emma::session_command::Flow::Continue => continue,
            }
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

        // `UserPromptSubmit`: once, here, on the words a person typed. After
        // command expansion so a hook reads what the model will read, and
        // before the goal exists so a delegation's brief can never reach it —
        // `delegate.rs` builds its own `Goal`, which is what makes "never for a
        // subagent" a property of the call graph rather than a flag to
        // remember.
        let submitted = harness
            .on_user_prompt(&text, &session_id, &log.path().display().to_string())
            .await;
        for run in &submitted.runs {
            log.append("hook", serde_json::json!({ "run": run }));
        }
        // A hook that failed loses its enrichment and says so. Silence here
        // would be a turn that quietly lacks the context the project expects.
        for notice in submitted.notices() {
            term.note(&notice);
        }
        if let Some(reason) = submitted.blocked {
            // Refused, but not vanished. The record is what stops a blocked
            // prompt from being a turn nobody can account for afterwards —
            // the same argument `prompt_blocked` makes against a silent drain.
            log.append(
                "prompt_blocked",
                serde_json::json!({ "text": text, "reason": reason }),
            );
            term.warn(&format!("prompt blocked: {reason}"));
            if opts.print {
                // A script must not read a blocked prompt as a finished goal.
                last = Ending::Interrupted;
                break;
            }
            continue;
        }

        // A goal starts un-interrupted, unless this run has only the one goal.
        // The rule and its reasoning are `Interrupt::starting_goal`, in the
        // library, where a test can reach both halves of it; it used to be the
        // `if` here, and the `-p` half was asserted nowhere.
        interrupt.starting_goal(opts.print);
        let outcome = agent
            .run_goal(&Goal::new(text).with_injected(submitted.context))
            .await;
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
        // **A browser session is scoped to the goal that opened it.** Carrying a
        // live one into the next goal would mean a page loaded under one goal's
        // approval grant is readable under the next one's, with nothing on
        // screen saying so — and Emma's per-host grants are already
        // session-scoped for exactly that reason. Announced rather than silent,
        // because a research task that had a login now does not.
        if let Some(pool) = &browser_pool {
            let closed = pool.close_all().await;
            if !closed.is_empty() {
                term.note(&format!(
                    "closed {} browser session(s) at the end of this goal — a session does not \
                     carry across goals",
                    closed.len()
                ));
            }
            // **A profile that could not be removed is said out loud.** It
            // holds the session's cookies, and this pool's own module doc is
            // that a browser Emma opened may hold a login.
            // `sweep_stale_profiles` clears it at the next start, which is a
            // backstop rather than a reason for the user not to know it is
            // there now.
            // The text lives with the pool, not here, because here it could not
            // be tested: reaching this line needs a real browser and a really
            // stranded profile. See `pool::leaked_profile_warning`.
            if let Some(warning) =
                emma_tools_web::browser::pool::leaked_profile_warning(&pool.leaked_profiles())
            {
                term.warn(&warning);
            }
        }
        // **A transcript that stopped being written is said out loud.** The
        // flag was already correct and nothing read it; the only signal was an
        // `eprintln!` whose visibility under the frame is unestablished
        // (`DEF-022`). See `SessionLog::transcript_warning`.
        if let Some(warning) = log.transcript_warning() {
            term.warn(&warning);
        }
        // `-p` runs one goal and stops. Interactively, Ctrl-C interrupts the
        // goal and hands the prompt back, which is what `cli.rs` and the
        // session's opening note have always said it does; `/exit`, `/quit` and
        // EOF end the session.
        if opts.print {
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
        //
        // The same argument applies with teeth to the browser pool: `exit` runs
        // no destructors, so its `Drop` would not fire and every Chrome it
        // started would survive this process — with an unauthenticated debugging
        // port, which is the leak this crate has already suffered once. Killed
        // by pid here, synchronously, because there is nothing left to await on.
        if let Some(pool) = &browser_pool {
            pool.kill_all_now();
        }
        emma::term::restore_terminal();
        std::process::exit(1);
    }
    Ok(())
}
