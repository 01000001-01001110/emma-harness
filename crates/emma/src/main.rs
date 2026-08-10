//! Wiring, and nothing else. Every decision this binary makes lives in the
//! library beside it, where a scripted `Provider` can drive it without a
//! network, a terminal or a signal handler.

use std::sync::Arc;

use anyhow::{Context, Result};
use emma::agent::{Agent, Ending, Interrupt, Setup};
use emma::approval::{Approvals, Asker, Gate};
use emma::cli::{self, Command};
use emma::goal::{Goal, MarkerClaim};
use emma::session::SessionLog;
use emma::settings;
use emma::skill::Skill;
use emma::term::{LineSource, Term};
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

    // These four touch a file and a terminal and nothing else. Running them
    // without a runtime means a broken runtime can never be the thing that
    // stops somebody fixing their credentials.
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

    let (gate, asker) = match (opts.skip_permissions, opts.print) {
        (true, _) => (Gate::SkipAll, Asker::Scripted(Default::default())),
        (false, true) => (Gate::Unattended, Asker::Scripted(Default::default())),
        (false, false) => (Gate::Ask, Asker::Terminal(LineSource::stdin().into())),
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
    let key = auth::load_default()
        .map_err(|e| anyhow::anyhow!(emma::commands::rename_auth(&e.to_string())))?;
    let provider = AnthropicProvider::new(key, Some(model));

    let session_id = SessionLog::new_id();
    let log = match opts
        .session_dir
        .clone()
        .or_else(|| SessionLog::default_dir(home.as_deref()))
    {
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
        term.note(&format!("{}  session {}", provider.model_id(), log.path().display()));
    }

    let interrupt = Interrupt::new();
    interrupt.install();

    let budgets = opts.budgets;
    let mut agent = Agent::new(Setup {
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
        mode: if opts.print { Mode::Batch } else { Mode::Stream },
    });

    let Command::Run(seed) = cli.command else {
        unreachable!("every other command returned above")
    };

    let mut next = seed;
    let mut last = Ending::Done;
    loop {
        let raw = match next.take() {
            Some(text) => text,
            None => {
                term.goal_prompt();
                match approvals.read_line().await {
                    Some(line) if line.trim().is_empty() => continue,
                    Some(line) => line,
                    None => break,
                }
            }
        };
        if matches!(raw.trim(), "/exit" | "/quit") {
            break;
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
        term.note(&format!(
            "{} — {} calls, {} tokens",
            outcome.ending.message(&budgets),
            outcome.iterations,
            outcome.tokens
        ));
        last = outcome.ending;
        if opts.print || interrupt.tripped() {
            break;
        }
    }

    // A goal that did not finish must not look like one that did to whatever
    // ran `emma -p` in a script.
    if opts.print && last != Ending::Done {
        std::process::exit(1);
    }
    Ok(())
}
