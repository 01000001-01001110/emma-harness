//! Delegation, driven by a scripted model on both sides of it.
//!
//! The whole feature is testable without a network because a subagent is
//! `Agent::run_goal` running a second time: the same `Fake` provider answers the
//! parent and then the child, in the order the two loops ask.
//!
//! Most of these were validated by mutation — the guarantee was removed from
//! `delegate.rs`, the test was watched to fail, and the guarantee was restored.
//! They are marked where they sit, with the line to delete. The one that
//! matters most is
//! [`the_footer_is_the_harnesss_record_and_not_the_subagents_account`]: make
//! `Facts::from` read the subagent's final message instead of the log and it
//! goes red, which is the difference between a record and a claim.
//!
//! **The last region is there because everything above it asserts on what came
//! back.** A delegation's `tool_result` cannot show the tool surface the
//! sub-run was offered, the system prompt it booted on, the brief it opened, or
//! the budget it was lent — and each of those was a one-line deletion away from
//! silent, with all fifteen of the tests above staying green. Six guarantees
//! were undefended that way: the agent file's tool list, the persona's
//! allowlist one level down, the argument check on the agent name, half the
//! remaining allowance, the ending written to the `delegation` record, and the
//! whole of what the caller tells the subagent.

mod support;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use emma::agent::{Agent, Budgets, Ending, Interrupt, Outcome, Running, Setup, Spend};
use emma::approval::{Answer, Approvals, Asker, Gate};
use emma::delegate::{Delegate, Nest};
use emma::goal::{DoneCheck, Goal, MarkerClaim};
use emma::session::SessionLog;
use emma::term::Term;
use emma_harness::{AgentDef, Flavor, Harness};
use emma_llm::{AssistantTurn, Caching, Event, LlmError, Message, Mode, Provider, Request};
use emma_tool_api::{Registry, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use support::{call, empty_harness, registry, text, Fake, Say, TestTool};

// region: Building one
// ---------------------------------------------------------------------------
// Building one
//
// A parent `Agent` whose registry holds a real `Delegate`, whose agent types
// hold real tools, and whose provider answers both loops from one script.
// ---------------------------------------------------------------------------

fn budgets() -> Budgets {
    Budgets {
        max_iterations: 20,
        max_tokens: 1_000_000,
        wall_clock: Duration::from_secs(60),
        max_kicks: 1,
        max_context: 1_000_000,
    }
}

/// An agent type that inherits everything the caller has.
fn agent_type(name: &str, tools: Option<Vec<&str>>) -> AgentDef {
    AgentDef {
        name: name.into(),
        description: format!("the {name} agent, for a test"),
        instructions: format!("You are {name}."),
        tools: tools.map(|t| t.into_iter().map(str::to_string).collect()),
        model: None,
        max_turns: None,
        max_tokens: None,
    }
}

struct Run {
    outcome: Outcome,
    conversation: Vec<Message>,
    parent_spend: i64,
}

/// Drive one parent goal whose registry contains `Delegate`.
///
/// Generic over the client rather than taking `Arc<Fake>`, so a test that has
/// to see what the *sub-run* was sent can pass a [`Watching`] wrapped around
/// the same script. Nothing else changes: `Fake` still satisfies it.
#[allow(clippy::too_many_arguments)]
async fn delegating(
    dir: &Path,
    provider: Arc<impl Provider + 'static>,
    defs: &[AgentDef],
    sub_tools: Vec<Arc<dyn Tool>>,
    approvals: Arc<Approvals>,
    budgets: Budgets,
    log: Arc<SessionLog>,
    term: Arc<Term>,
) -> Run {
    let root = empty_harness(dir);
    let harness = Arc::new(Harness::load_selecting(&root, Flavor::Emma, None).unwrap());
    let spend = Spend::new();
    let base: Arc<dyn Provider> = provider.clone();
    let (delegate, _notes) = Delegate::new(
        Nest {
            harness: harness.clone(),
            approvals: approvals.clone(),
            log: log.clone(),
            term: term.clone(),
            interrupt: Interrupt::new(),
            spend: spend.clone(),
            cwd: dir.to_path_buf(),
            session_id: "sess-test".into(),
            caching: Caching::On,
            budgets,
            running: Running::new(base.clone()),
        },
        defs,
        &sub_tools,
        None,
        // `None` for every type: no agent file in these fixtures names a model,
        // and `None` is what "use the parent's, whatever it is now" means.
        &|_| None,
    );
    let tools = registry(vec![
        Arc::new(delegate.expect("no agent types resolved")) as Arc<dyn Tool>
    ]);

    let mut agent = Agent::new(Setup {
        background: Default::default(),
        provider: base.clone(),
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals: &approvals,
        log: &log,
        term: &term,
        interrupt: Interrupt::new(),
        spend: spend.clone(),
        done: &MarkerClaim,
        cwd: dir.to_path_buf(),
        session_id: "sess-test".into(),
        budgets,
        caching: Caching::On,
        mode: Mode::Batch,
    });
    let outcome = agent
        .run_goal(&Goal::new("find out where the retry policy lives"))
        .await;
    Run {
        conversation: agent.conversation(),
        parent_spend: spend.get(),
        outcome,
    }
}

fn delegate_to(agent: &str, task: &str) -> Say {
    call("Delegate", json!({ "agent": agent, "task": task }))
}

/// Everything the parent's model was ever sent — which is where a `tool_result`
/// carrying a footer shows up.
fn allowing_everything() -> Arc<Approvals> {
    Arc::new(Approvals::new(
        Gate::SkipAll,
        Asker::Scripted(Default::default()),
    ))
}

// endregion: Building one

// region: End to end
// ---------------------------------------------------------------------------
// End to end
//
// What a delegation looks like from the parent's side: one tool call in, one
// message plus a record out.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_delegation_runs_a_nested_loop_and_returns_its_conclusion_under_a_footer() {
    let dir = tempfile::tempdir().unwrap();
    let (read, read_calls) = TestTool::returning("Read", "fn retry() {}");
    let fake = Fake::new(vec![
        // The parent delegates…
        delegate_to("explorer", "where is the retry policy decided?"),
        // …the sub reads a file and answers…
        call("Read", json!({ "file_path": "src/retry.rs" })),
        text("It is decided in src/retry.rs.\n\nGOAL COMPLETE"),
        // …and the parent finishes on what came back.
        text("The retry policy lives in src/retry.rs.\n\nGOAL COMPLETE"),
    ]);

    let run = delegating(
        dir.path(),
        fake.clone(),
        &[agent_type("explorer", Some(vec!["Read"]))],
        vec![read],
        allowing_everything(),
        budgets(),
        Arc::new(SessionLog::none()),
        Arc::new(Term::silent()),
    )
    .await;

    assert_eq!(run.outcome.ending, Ending::Done);
    // The sub really ran: its tool was called, by it, not by the parent.
    assert_eq!(read_calls.load(Ordering::SeqCst), 1);

    let seen = fake.transcript();
    // The sub's own words reached the parent…
    assert!(seen.contains("It is decided in src/retry.rs."), "{seen}");
    // …under a footer the sub did not write.
    assert!(seen.contains("ended: done"), "{seen}");
    assert!(seen.contains("files touched (1)"), "{seen}");
    assert!(seen.contains("recorded by the harness"), "{seen}");
}

/// A command that **succeeded** reaches the footer with its status.
///
/// **The half of `DEF-004` that no test could see.** The footer reads
/// `ToolOutcome::exit_code` and falls back to parsing the tool's prose. Real
/// `Bash` writes `exit status <n>` only when `!status.success()` — so for every
/// command that worked, the prose carries no status at all and the fallback
/// returns `None`. The footer then renders `no result (refused, or the run
/// stopped)` for a command that ran and exited 0, which is the opposite of
/// what happened.
///
/// Every fixture in this file predates the field, so deleting the structural
/// read left all fourteen tests green. An independent reviewer found that by
/// deleting it. The fixture here sets the field and states nothing in its
/// prose, which is what makes the two paths distinguishable.
#[tokio::test]
async fn a_command_that_succeeded_is_not_reported_as_no_result() {
    let dir = tempfile::tempdir().unwrap();
    // No `exit status` line: exactly what Bash writes on success.
    let (bash, bash_calls) = TestTool::ran(
        "Bash",
        "shell: bash
all tests passed
",
        0,
    );
    let fake = Fake::new(vec![
        delegate_to("runner", "run the tests"),
        call("Bash", json!({ "command": "cargo test" })),
        text(
            "They pass.

GOAL COMPLETE",
        ),
        text(
            "Done.

GOAL COMPLETE",
        ),
    ]);

    let run = delegating(
        dir.path(),
        fake.clone(),
        &[agent_type("runner", Some(vec!["Bash"]))],
        vec![bash],
        allowing_everything(),
        budgets(),
        Arc::new(SessionLog::none()),
        Arc::new(Term::silent()),
    )
    .await;

    assert_eq!(run.outcome.ending, Ending::Done);
    assert_eq!(
        bash_calls.load(Ordering::SeqCst),
        1,
        "the sub did not run the command, so the footer has nothing to report on"
    );

    let seen = fake.transcript();
    assert!(
        seen.contains("cargo test"),
        "the command is missing from the footer: {seen}"
    );
    assert!(
        !seen.contains("no result"),
        "a command that ran and exited 0 was reported as having no result, which \
         is what the footer says about a command that was refused or never \
         finished: {seen}"
    );
    assert!(
        seen.contains("cargo test → exit 0") || seen.contains("cargo test → 0"),
        "the footer did not carry the exit status it was handed: {seen}"
    );
}

/// **The test the whole design turns on, and the one to mutate.**
///
/// The scripted subagent claims, in perfectly plausible prose, to have read two
/// files and run a command that passed. It called no tool at all. Compose the
/// footer from `outcome.text` rather than from the run's records and this goes
/// red: the claimed paths appear under `files touched`, and `cargo test → exit 0`
/// appears under `commands run`.
///
/// Note what is *not* asserted: that the claim was removed. It is still there,
/// verbatim, because the subagent's own account is what the parent asked for.
/// What the footer adds is the means to check it.
#[tokio::test]
async fn the_footer_is_the_harnesss_record_and_not_the_subagents_account() {
    let dir = tempfile::tempdir().unwrap();
    let (read, read_calls) = TestTool::returning("Read", "never called");
    let fake = Fake::new(vec![
        delegate_to("explorer", "check the retry tests"),
        text(
            "I read src/retry.rs and tests/retry.rs, and ran `cargo test` — exit status 0, \
             everything passes.\n\nGOAL COMPLETE",
        ),
        text("Done.\n\nGOAL COMPLETE"),
    ]);

    let run = delegating(
        dir.path(),
        fake.clone(),
        &[agent_type("explorer", Some(vec!["Read"]))],
        vec![read],
        allowing_everything(),
        budgets(),
        Arc::new(SessionLog::none()),
        Arc::new(Term::silent()),
    )
    .await;
    assert_eq!(run.outcome.ending, Ending::Done);
    assert_eq!(
        read_calls.load(Ordering::SeqCst),
        0,
        "the sub called nothing"
    );

    let seen = fake.transcript();
    assert!(
        seen.contains("I read src/retry.rs"),
        "the claim was censored: {seen}"
    );
    assert!(
        seen.contains("files touched: none"),
        "the footer credited files nothing opened: {seen}"
    );
    assert!(
        seen.contains("commands run: none"),
        "the footer credited a command nothing ran: {seen}"
    );
    assert!(
        !seen.contains("cargo test → exit"),
        "the footer echoed the model's account of a command: {seen}"
    );
}

/// `Ending::Tokens` and `Ending::Done` leave the same confident final paragraph.
/// Without the footer the parent cannot tell them apart, so this asserts the
/// difference is on the wire — and that the partial answer is carried rather
/// than thrown away.
#[tokio::test]
async fn a_subagent_that_ran_out_of_budget_is_not_reported_as_a_finished_one() {
    let dir = tempfile::tempdir().unwrap();
    let (read, _) = TestTool::returning("Read", "some file");
    let mut ty = agent_type("explorer", Some(vec!["Read"]));
    ty.max_tokens = Some(5_000);
    let fake = Fake::new(vec![
        delegate_to("explorer", "read everything"),
        // One expensive call, over the type's own cap.
        call("Read", json!({ "file_path": "src/big.rs" })).costing(9_000),
        text("Understood.\n\nGOAL COMPLETE"),
    ]);

    let run = delegating(
        dir.path(),
        fake.clone(),
        &[ty],
        vec![read],
        allowing_everything(),
        budgets(),
        Arc::new(SessionLog::none()),
        Arc::new(Term::silent()),
    )
    .await;
    assert_eq!(run.outcome.ending, Ending::Done);

    let seen = fake.transcript();
    assert!(seen.contains("ended: tokens"), "{seen}");
    assert!(seen.contains("as partial"), "{seen}");
    // A failed delegation is a `tool_result` the model routes around, never an
    // abort — the governing rule of the whole loop, applied without exception.
    assert!(seen.contains("is_error"), "{seen}");
    assert!(seen.contains("Re-issuing the same brief"), "{seen}");
}

/// One meter, in the strongest sense available: the same integer. Without the
/// shared `Spend` the parent's cap would bound only the parent's own calls, and
/// a delegation would be a way to spend past it.
#[tokio::test]
async fn a_subagents_spend_charges_the_parents_meter() {
    let dir = tempfile::tempdir().unwrap();
    let (read, _) = TestTool::returning("Read", "x");
    let fake = Fake::new(vec![
        delegate_to("explorer", "look").costing(100),
        call("Read", json!({ "file_path": "a.rs" })).costing(7_000),
        text("found it\n\nGOAL COMPLETE").costing(300),
        text("ok\n\nGOAL COMPLETE").costing(100),
    ]);

    let run = delegating(
        dir.path(),
        fake.clone(),
        &[agent_type("explorer", Some(vec!["Read"]))],
        vec![read],
        allowing_everything(),
        budgets(),
        Arc::new(SessionLog::none()),
        Arc::new(Term::silent()),
    )
    .await;

    // 100 + 100 from the parent's two calls, 7,300 from the sub's.
    assert_eq!(run.parent_spend, 7_500);
    assert_eq!(run.outcome.tokens, 7_500);
}

// endregion: End to end

// region: The three things that would bite
// ---------------------------------------------------------------------------
// The three things that would bite
//
// Recursion, the session file, and two prompts on one keyboard.
// ---------------------------------------------------------------------------

/// Recursion is prevented by the child registry not containing `Delegate`, so
/// the hazard is a future edit that builds that registry from the parent's. This
/// is written the same way `approval.rs`'s exemption test is, and for the same
/// reason: the failure is what somebody later *adds*.
#[tokio::test]
async fn an_agent_type_cannot_be_given_the_delegate_tool_even_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let (read, _) = TestTool::returning("Read", "x");
    let fake = Fake::new(vec![
        delegate_to("explorer", "delegate further, if you can"),
        // The inner model tries. There is nothing to call.
        call("Delegate", json!({ "agent": "explorer", "task": "deeper" })),
        text("I cannot delegate from here.\n\nGOAL COMPLETE"),
        text("Right.\n\nGOAL COMPLETE"),
    ]);

    let run = delegating(
        dir.path(),
        fake.clone(),
        // The file names it explicitly. It still cannot have it: the tool does
        // not exist when the child registry is built.
        &[agent_type("explorer", Some(vec!["Read", "Delegate"]))],
        vec![read],
        allowing_everything(),
        budgets(),
        Arc::new(SessionLog::none()),
        Arc::new(Term::silent()),
    )
    .await;

    assert_eq!(run.outcome.ending, Ending::Done);
    let seen = fake.transcript();
    assert!(seen.contains("no_such_tool"), "{seen}");
    assert!(
        seen.contains("`Delegate` is not available"),
        "the inner model got something other than the ordinary unknown-tool observation: {seen}"
    );
}

/// The fold test the mechanism note asks for by name.
///
/// A sub record written under the parent's own `kind` names would make
/// `Fold::close_turn` place the parent's held turn without its results —
/// `answered()` fails, `place_turn` returns false, and the parent's whole turn
/// is silently dropped from every resume. Nothing else in the suite would
/// notice.
#[tokio::test]
async fn folding_a_session_containing_a_delegation_returns_the_parents_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(SessionLog::open(dir.path(), "sess-delegation").unwrap());
    let (read, _) = TestTool::returning("Read", "fn retry() {}");
    let fake = Fake::new(vec![
        delegate_to("explorer", "where is retry decided?"),
        call("Read", json!({ "file_path": "src/retry.rs" })),
        text("src/retry.rs.\n\nGOAL COMPLETE"),
        text("It is in src/retry.rs.\n\nGOAL COMPLETE"),
    ]);

    let run = delegating(
        dir.path(),
        fake.clone(),
        &[agent_type("explorer", Some(vec!["Read"]))],
        vec![read],
        allowing_everything(),
        budgets(),
        log.clone(),
        Arc::new(Term::silent()),
    )
    .await;
    assert_eq!(run.outcome.ending, Ending::Done);

    let folded = emma::session::fold(log.path()).unwrap();
    assert_eq!(
        folded, run.conversation,
        "folding a session with a delegation in it did not reproduce the parent's conversation"
    );
    // The sub's traffic really is in the same file — the audit trail is not the
    // thing being protected here, the parent's fold is.
    let records = SessionLog::read(log.path()).unwrap();
    let kinds: Vec<&str> = records.iter().filter_map(|r| r["kind"].as_str()).collect();
    assert!(kinds.contains(&"sub.goal"), "{kinds:?}");
    assert!(kinds.contains(&"sub.tool_call"), "{kinds:?}");
    assert!(kinds.contains(&"delegation"), "{kinds:?}");
    // Every sub record says which delegation wrote it and which parent turn
    // asked, which is what makes one file separable by `grep`.
    for record in records
        .iter()
        .filter(|r| r["kind"].as_str().is_some_and(|k| k.starts_with("sub.")))
    {
        assert!(record["sub_id"].is_string(), "{record}");
        assert!(record["parent_turn_id"].is_string(), "{record}");
    }
}

/// A tool that reports the highest number of concurrent calls it ever saw.
struct Overlapping {
    live: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for Overlapping {
    fn name(&self) -> &'static str {
        "Read"
    }
    fn description(&self) -> &str {
        "counts how many callers are inside it at once"
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object" })
    }
    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: true,
            reaches_network: false,
            idempotent: true,
        }
    }
    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        _args: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(60)).await;
        self.live.fetch_sub(1, Ordering::SeqCst);
        Ok(Ok(ToolOutcome::new("read")))
    }
}

/// **The one-at-a-time rule, and what enforces it.**
///
/// Two `invoke` calls launched together. The permit is what keeps the second
/// waiting; without it both nested runs would be inside the tool at once and the
/// peak would be 2 — and, on a real terminal, two approval prompts would be
/// racing for one stdin, which is the `y`-answers-the-wrong-question defect
/// `LineSource::drain` exists to prevent.
///
/// Mutation-checked: removing `let _permit = self.permit.acquire().await` makes
/// this fail.
#[tokio::test]
async fn two_delegations_never_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let harness = Arc::new(Harness::load_selecting(&root, Flavor::Emma, None).unwrap());
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let tool: Arc<dyn Tool> = Arc::new(Overlapping {
        live: live.clone(),
        peak: peak.clone(),
    });
    // Two tool calls at the front of the script, so that *whichever* nested run
    // reaches the model first, the next one to reach it also gets a tool call
    // and lands inside `Overlapping` while the first is still there. A script
    // whose second entry was a text turn would let the second run finish without
    // ever entering the tool, and the test would pass with the permit removed —
    // which it did, before this comment was written.
    let fake = Fake::new(vec![
        call("Read", json!({ "file_path": "a" })),
        call("Read", json!({ "file_path": "b" })),
        text("done\n\nGOAL COMPLETE"),
        text("done\n\nGOAL COMPLETE"),
    ]);
    let base: Arc<dyn Provider> = fake.clone();
    let (delegate, _) = Delegate::new(
        Nest {
            harness,
            approvals: allowing_everything(),
            log: Arc::new(SessionLog::none()),
            term: Arc::new(Term::silent()),
            interrupt: Interrupt::new(),
            spend: Spend::new(),
            cwd: dir.path().to_path_buf(),
            session_id: "sess-test".into(),
            caching: Caching::On,
            budgets: budgets(),
            running: Running::new(base.clone()),
        },
        &[agent_type("explorer", Some(vec!["Read"]))],
        &[tool],
        None,
        &|_| None,
    );
    let delegate = delegate.unwrap();
    let ctx = ToolCtx {
        cwd: dir.path().to_path_buf(),
        session_id: "sess-test".into(),
        turn_id: "turn-1".into(),
        background: Default::default(),
    };
    let one = delegate.invoke(&ctx, json!({ "agent": "explorer", "task": "a" }));
    let two = delegate.invoke(&ctx, json!({ "agent": "explorer", "task": "b" }));
    let (a, b) = tokio::join!(one, two);
    assert!(a.unwrap().is_ok());
    assert!(b.unwrap().is_ok());
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "two subagents were inside the tool surface at the same time"
    );
}

// endregion: The three things that would bite

// region: The status meters
// ---------------------------------------------------------------------------
// The status meters
//
// The latent bug, in both halves: the constructor that used to move them, and
// the nested run that would keep moving them.
// ---------------------------------------------------------------------------

/// `Agent::new` used to call `term.set_budgets(...)`. Constructing a nested
/// `Agent` therefore re-pointed the process's context and token meters at a
/// sub-run's caps for the rest of the run — a meter measured against the wrong
/// cap, which is exactly the untruth the status line exists to not have.
#[tokio::test]
async fn constructing_an_agent_never_moves_the_status_meters() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let harness = Harness::load_selecting(&root, Flavor::Emma, None).unwrap();
    let term = Term::recording();
    let tools = Registry::new();
    let approvals = Approvals::unattended();
    let fake = Fake::new(Vec::new());
    let log = SessionLog::none();
    let _agent = Agent::new(Setup {
        background: Default::default(),
        provider: fake.clone(),
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals: &approvals,
        log: &log,
        term: &term,
        interrupt: Interrupt::new(),
        spend: Spend::new(),
        done: &MarkerClaim,
        cwd: dir.path().to_path_buf(),
        session_id: "sess-test".into(),
        budgets: budgets(),
        caching: Caching::On,
        mode: Mode::Batch,
    });
    assert!(
        term.recorded().is_empty(),
        "constructing an Agent touched the status line: {:?}",
        term.recorded()
    );
}

/// The other three of the same shape: a nested `goal_started`/`goal_ended` stops
/// the parent's clock while the parent is still running, and a nested `spent`
/// feeds the live line a per-goal figure smaller than the parent's, so the meter
/// jumps backwards mid-goal.
///
/// The approval prompt is asserted in the same test on purpose. A subordinate
/// terminal that muted it would silently convert every sub-approval into a hang
/// or a denial, and a test that only checked the meters were quiet would pass.
#[tokio::test]
async fn a_nested_run_moves_no_meter_and_still_reaches_the_keyboard() {
    let dir = tempfile::tempdir().unwrap();
    let term = Arc::new(Term::recording());
    let (write, write_calls) = TestTool::ok("Write", false);
    let fake = Fake::new(vec![
        delegate_to("implementer", "add the file"),
        call("Write", json!({ "file_path": "a.rs", "content": "x" })),
        text("written\n\nGOAL COMPLETE"),
        text("done\n\nGOAL COMPLETE"),
    ]);

    // The parent's own meters, moved once before the run, exactly as `main` does.
    term.set_budgets(budgets().max_context, budgets().max_tokens);
    let approvals = Arc::new(Approvals::new(
        Gate::Ask,
        // Two: the delegation itself is gated like any other writer, and then
        // the subagent's `Write` is gated again — on the same terminal, from
        // the same queue.
        Asker::Scripted(vec![Answer::Yes, Answer::Yes].into()),
    ));
    let run = delegating(
        dir.path(),
        fake.clone(),
        &[agent_type("implementer", Some(vec!["Write"]))],
        vec![write],
        approvals,
        budgets(),
        Arc::new(SessionLog::none()),
        term.clone(),
    )
    .await;
    assert_eq!(run.outcome.ending, Ending::Done);
    assert_eq!(
        write_calls.load(Ordering::SeqCst),
        1,
        "the sub's write did not run"
    );

    let said = term.recorded();
    // The sub's write was gated, on the parent's terminal, with the parent's
    // keyboard answering it.
    assert_eq!(
        said.iter().filter(|s| *s == "prompt_header").count(),
        2,
        "the delegation and the subagent's write were not both put to the human: {said:?}"
    );
    // The parent's own goal moved the meters exactly as many times as the parent
    // has goals and model calls — one `set_budgets`, one `goal_started`, one
    // `goal_ended`, one `spent` per parent call. A nested run adding its own is
    // the bug.
    let count = |what: &str| said.iter().filter(|s| *s == what).count();
    assert_eq!(count("set_budgets"), 1, "{said:?}");
    assert_eq!(count("goal_started"), 1, "{said:?}");
    assert_eq!(count("goal_ended"), 1, "{said:?}");
    assert_eq!(
        count("spent"),
        2,
        "the sub fed the parent's meter: {said:?}"
    );
}

// endregion: The status meters

// region: The gate, the budget and the resume
// ---------------------------------------------------------------------------
// The gate, the budget and the resume
//
// Three narrow rulings that would each be silent if they broke.
// ---------------------------------------------------------------------------

#[test]
fn delegation_is_gated_and_the_prompt_shows_the_brief() {
    // `read_only: false` unconditionally, including for an agent type whose
    // tools are entirely read-only: the tool set is configuration, and a
    // `meta()` that varied with it would put the gate's answer in a file the
    // reviewer is not looking at.
    let preview = emma::approval::preview(
        "Delegate",
        &json!({
            "agent": "explorer",
            "task": "find every call site of the old constructor",
            "context": ["src/a.rs is the caller", "cargo build already fails"],
            "deliver": "the paths and line numbers",
        }),
    );
    assert!(preview.contains("explorer"), "{preview}");
    assert!(preview.contains("old constructor"), "{preview}");
    assert!(preview.contains("2 facts"), "{preview}");
    assert!(preview.contains("the paths and line numbers"), "{preview}");
    // The fallback would have been pretty-printed JSON, which is the prompt
    // people learn to approve without reading.
    assert!(!preview.contains('{'), "{preview}");
}

#[tokio::test]
async fn a_delegation_refuses_to_start_when_the_goal_has_almost_nothing_left() {
    // A delegation launched with nothing left returns a good answer into a goal
    // that immediately ends, and the work is lost though it was paid for. The
    // refusal is a `tool_result` the model can route around, never a missing
    // tool — a capability that silently disappears late in every goal is a trap
    // of a different shape.
    let dir = tempfile::tempdir().unwrap();
    let (read, read_calls) = TestTool::returning("Read", "x");
    let fake = Fake::new(vec![
        delegate_to("explorer", "look at everything"),
        text("I will do it here instead.\n\nGOAL COMPLETE"),
    ]);
    let tight = Budgets {
        max_tokens: 1_000,
        ..budgets()
    };

    let run = delegating(
        dir.path(),
        fake.clone(),
        &[agent_type("explorer", Some(vec!["Read"]))],
        vec![read],
        allowing_everything(),
        tight,
        Arc::new(SessionLog::none()),
        Arc::new(Term::silent()),
    )
    .await;
    assert_eq!(run.outcome.ending, Ending::Done);
    assert_eq!(
        read_calls.load(Ordering::SeqCst),
        0,
        "a subagent started anyway"
    );
    let seen = fake.transcript();
    assert!(seen.contains("tool_unavailable"), "{seen}");
    assert!(seen.contains("I cannot start a delegation"), "{seen}");
}

/// The measurement, end to end: a delegation happens, and a person can ask what
/// it cost without opening a JSONL file.
///
/// It reads the same records the footer is built from, so the two cannot
/// disagree about what happened — and it calls no model, which is what makes it
/// safe to run while wondering whether delegation is worth it at all.
#[tokio::test]
async fn what_a_delegation_cost_can_be_read_back_across_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let log = Arc::new(SessionLog::open(&sessions, "sess-0000000000001-1").unwrap());
    let (read, _) = TestTool::returning("Read", "fn retry() {}");
    let fake = Fake::new(vec![
        delegate_to("explorer", "where is retry decided?").costing(50),
        call("Read", json!({ "file_path": "src/retry.rs" })).costing(2_000),
        text("src/retry.rs:14.\n\nGOAL COMPLETE").costing(300),
        text("It is at src/retry.rs:14.\n\nGOAL COMPLETE").costing(50),
    ]);
    let run = delegating(
        dir.path(),
        fake.clone(),
        &[agent_type("explorer", Some(vec!["Read"]))],
        vec![read],
        allowing_everything(),
        budgets(),
        log.clone(),
        Arc::new(Term::silent()),
    )
    .await;
    assert_eq!(run.outcome.ending, Ending::Done);

    let mut out = Vec::new();
    emma::commands::agents(Some(&sessions), &mut out).unwrap();
    let report = String::from_utf8(out).unwrap();
    assert!(report.contains("explorer"), "{report}");
    assert!(report.contains("done 1"), "{report}");
    assert!(report.contains("delegations    1"), "{report}");
    // The spend is the sub's own, taken off its record rather than off the
    // parent's total: 2,300 for its two calls.
    assert!(report.contains("2300"), "{report}");
    assert!(report.contains("where is retry decided?"), "{report}");

    // **And a session file that cannot be read is counted, not skipped in
    // silence.** This command's whole output is totals: a session nobody could
    // read contributes nothing to them and the numbers still look like an
    // answer. A reviewer found the bare `continue` and pointed out that turning
    // it into a `break` left every test green, the fixtures all being clean
    // UTF-8. These bytes are not.
    std::fs::write(sessions.join("sess-0000000000002-1.jsonl"), [0xFFu8, 0xFE]).unwrap();
    let mut out = Vec::new();
    emma::commands::agents(Some(&sessions), &mut out).unwrap();
    let report = String::from_utf8(out).unwrap();
    assert!(
        report.contains("could not be read"),
        "a session file was passed over and the totals were printed as though          they were complete:
{report}"
    );
    assert!(
        report.contains("sess-0000000000002-1"),
        "the unreadable file is not named, so nobody can go and look at it:
{report}"
    );
    // The totals still come out. An unreadable file is a gap in the answer, not
    // a reason to refuse one.
    assert!(report.contains("explorer"), "{report}");
}

#[test]
fn asking_what_delegation_cost_before_delegating_anything_says_so() {
    // The empty case is a real one — most sessions never delegate — and a table
    // of zeroes reads as a broken command.
    let dir = tempfile::tempdir().unwrap();
    let mut out = Vec::new();
    emma::commands::agents(Some(dir.path()), &mut out).unwrap();
    let report = String::from_utf8(out).unwrap();
    assert!(report.contains("Nothing has been delegated"), "{report}");
    assert!(report.contains("emma config check"), "{report}");
}

#[test]
fn a_resumed_parent_gets_back_what_its_delegations_spent() {
    // The sub's own `sub.model_call` records are namespaced and invisible to
    // `restore_records`, so without the `delegation` arm a resumed parent would
    // come back with a meter missing everything its subagents spent — which is a
    // way to spend past a cap.
    let restored = emma::session::restore_records(&[
        json!({ "kind": "goal", "text": "g", "opening": "g" }),
        json!({ "kind": "model_call", "cost_tokens": 100 }),
        json!({ "kind": "delegation", "agent": "explorer", "cost_tokens": 7_400 }),
        json!({ "kind": "model_call", "cost_tokens": 100 }),
    ]);
    assert_eq!(restored.tokens, 7_600);
    // …and it must not inflate the call count: the parent made one model call
    // for that turn, not fifteen.
    assert_eq!(restored.iterations, 2);
}

// endregion: The gate, the budget and the resume

// region: What the sub-run is actually given
// ---------------------------------------------------------------------------
// What the sub-run is actually given
//
// Everything above asserts on what came *back* from a delegation. Five of the
// guarantees in this file are about what goes *in* — the tool surface, the
// system prompt, the brief and the budget — and none of them is visible in a
// `tool_result`. Deleting the line that enforces any of them left all fifteen
// tests above green, which is what this region exists to change.
// ---------------------------------------------------------------------------

/// A client that forwards to a scripted one and keeps what each request carried.
///
/// It exists because `Fake::transcript` renders the *messages*, and the
/// guarantees below live in the other two fields: `instructions` is the system
/// prompt a sub-run was booted with, and `tools` is the closed set it was
/// offered. A test that can only read messages cannot tell an agent type handed
/// one tool from one handed every tool the process has.
struct Watching {
    inner: Arc<Fake>,
    /// The system prompt of every request, oldest first. A delegation produces
    /// the parent's and then the sub's, and they must differ.
    prompts: Mutex<Vec<String>>,
    /// The tool names offered on every request, in the same order.
    surfaces: Mutex<Vec<Vec<String>>>,
    /// Streaming or not, per request. A sub-run reports once at the end and a
    /// subordinate `Term` swallows prose, so streaming one is bytes nobody
    /// reads — and nothing else in this file can see which was asked for.
    modes: Mutex<Vec<Mode>>,
}

impl Watching {
    fn over(inner: Arc<Fake>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            prompts: Mutex::default(),
            surfaces: Mutex::default(),
            modes: Mutex::default(),
        })
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }

    fn surfaces(&self) -> Vec<Vec<String>> {
        self.surfaces.lock().unwrap().clone()
    }

    fn modes(&self) -> Vec<Mode> {
        self.modes.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Provider for Watching {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    async fn send(
        &self,
        request: Request,
        mode: Mode,
        events: Option<tokio::sync::mpsc::Sender<Event>>,
    ) -> Result<AssistantTurn, LlmError> {
        self.prompts
            .lock()
            .unwrap()
            .push(request.instructions.clone());
        self.surfaces.lock().unwrap().push(
            request
                .tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(str::to_string))
                .collect(),
        );
        self.modes.lock().unwrap().push(mode);
        self.inner.send(request, mode, events).await
    }
}

/// **What a subagent may use is what its agent file says, not what it asks
/// for.**
///
/// Mutation-checked: make `resolve_tools` register every candidate instead of
/// the ones the file named — one line — and this goes red twice over. The
/// closed tool set is the security property the module doc names beside the
/// closed agent enum: an agent type that can be talked into a tool the operator
/// did not give it is a free-text tool list wearing a name.
///
/// The `Read` assertion is the control. Without it a `resolve_tools` that
/// returned an empty registry for everything would satisfy the interesting
/// half, and "no tools at all" is a different defect rather than this guarantee
/// holding.
#[tokio::test]
async fn a_subagent_is_offered_only_the_tools_its_agent_file_names() {
    let dir = tempfile::tempdir().unwrap();
    let (read, read_calls) = TestTool::returning("Read", "fn retry() {}");
    let (write, write_calls) = TestTool::ok("Write", false);
    let fake = Fake::new(vec![
        delegate_to("explorer", "read a file, and try to write one"),
        // The inner model asks for a tool its file did not name.
        call("Write", json!({ "file_path": "a.rs", "content": "x" })),
        call("Read", json!({ "file_path": "src/retry.rs" })),
        text("I could read but not write.\n\nGOAL COMPLETE"),
        text("Understood.\n\nGOAL COMPLETE"),
    ]);
    let provider = Watching::over(fake.clone());

    let run = delegating(
        dir.path(),
        provider.clone(),
        &[agent_type("explorer", Some(vec!["Read"]))],
        // Both tools exist in this build and both were handed to `Delegate`.
        // The agent file is the only thing standing between the sub and `Write`.
        vec![read, write],
        allowing_everything(),
        budgets(),
        Arc::new(SessionLog::none()),
        Arc::new(Term::silent()),
    )
    .await;
    assert_eq!(run.outcome.ending, Ending::Done);
    assert_eq!(
        write_calls.load(Ordering::SeqCst),
        0,
        "the sub reached a tool its agent file does not name"
    );
    assert_eq!(
        read_calls.load(Ordering::SeqCst),
        1,
        "the tool the file *did* name never ran either, so this proves nothing \
         about the filter"
    );

    // The surface itself, which is what the filter decides. Every request in
    // this run is one of two: the parent's, holding `Delegate`, and the sub's,
    // which must hold exactly what its file named.
    let surfaces = provider.surfaces();
    let sub: Vec<&Vec<String>> = surfaces
        .iter()
        .filter(|s| !s.iter().any(|n| n == "Delegate"))
        .collect();
    assert!(!sub.is_empty(), "no sub-run request was captured");
    for surface in sub {
        assert_eq!(
            surface,
            &vec!["Read".to_string()],
            "the sub-run was offered a tool surface its agent file did not name"
        );
    }

    let seen = fake.transcript();
    assert!(
        seen.contains("`Write` is not available"),
        "the inner model got something other than the ordinary unknown-tool \
         observation for a tool it was not given: {seen}"
    );
}

/// **The persona's allowlist reaches one level down.**
///
/// An agent type must not be a route to a tool the operator excluded from this
/// run. Mutation-checked: drop the `persona_allowed` filter in `Delegate::new`
/// — one closure — and this goes red while every other test stays green,
/// because every other test passes `None` for it.
///
/// The `None` half is the control, and it is what proves the `Some` half
/// narrowed the set rather than the fixture never having offered `Write` at
/// all.
#[test]
fn an_agent_type_cannot_reach_a_tool_the_persona_excluded() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let harness = Arc::new(Harness::load_selecting(&root, Flavor::Emma, None).unwrap());
    let (read, _) = TestTool::returning("Read", "x");
    let (write, _) = TestTool::ok("Write", false);
    let available = vec![read, write];
    // The file asks for both, every time. Only the persona's list changes.
    let defs = [agent_type("explorer", Some(vec!["Read", "Write"]))];

    let tools_of = |allowed: Option<&[String]>| {
        let fake = Fake::new(Vec::new());
        let base: Arc<dyn Provider> = fake.clone();
        let (delegate, _) = Delegate::new(
            Nest {
                harness: harness.clone(),
                approvals: allowing_everything(),
                log: Arc::new(SessionLog::none()),
                term: Arc::new(Term::silent()),
                interrupt: Interrupt::new(),
                spend: Spend::new(),
                cwd: dir.path().to_path_buf(),
                session_id: "sess-test".into(),
                caching: Caching::On,
                budgets: budgets(),
                running: Running::new(base),
            },
            &defs,
            &available,
            allowed,
            &|_| None,
        );
        delegate
            .expect("no agent types resolved")
            .tools_of("explorer")
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<String>>()
    };

    // The control: with no persona list, the file gets what it asked for.
    assert_eq!(
        tools_of(None),
        vec!["Read".to_string(), "Write".to_string()],
        "the fixture never offered both, so the exclusion below would prove \
         nothing"
    );
    assert_eq!(
        tools_of(Some(&["Read".to_string()])),
        vec!["Read".to_string()],
        "an agent type was handed a tool the operator excluded from this run"
    );
}

/// **The agent name is a closed enum, enforced and not merely declared.**
///
/// `input_schema` states it, and a provider that ignores an enum must not turn
/// this into something that runs a prompt nobody wrote. Mutation-checked:
/// disable the `types.contains_key` refusal in `validate_args` and this goes
/// red, and nothing else in the suite notices — `invoke`'s own second check
/// keeps the end-to-end path working, which is exactly why the first one is
/// deletable in silence.
///
/// The accepted case is the control: a `validate_args` that refused everything
/// would satisfy the refusal on its own.
#[test]
fn an_unknown_agent_name_is_refused_by_the_argument_check_and_not_only_by_the_schema() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let harness = Arc::new(Harness::load_selecting(&root, Flavor::Emma, None).unwrap());
    let (read, _) = TestTool::returning("Read", "x");
    let fake = Fake::new(Vec::new());
    let base: Arc<dyn Provider> = fake.clone();
    let (delegate, _) = Delegate::new(
        Nest {
            harness,
            approvals: allowing_everything(),
            log: Arc::new(SessionLog::none()),
            term: Arc::new(Term::silent()),
            interrupt: Interrupt::new(),
            spend: Spend::new(),
            cwd: dir.path().to_path_buf(),
            session_id: "sess-test".into(),
            caching: Caching::On,
            budgets: budgets(),
            running: Running::new(base),
        },
        &[agent_type("explorer", Some(vec!["Read"]))],
        &[read],
        None,
        &|_| None,
    );
    let delegate = delegate.expect("no agent types resolved");

    // The control.
    assert!(delegate
        .validate_args(&json!({ "agent": "explorer", "task": "go and look" }))
        .is_ok());

    let refused = delegate
        .validate_args(&json!({ "agent": "explorer-2", "task": "go and look" }))
        .expect_err("a name that is not in the catalogue was accepted");
    let ToolError::BadArguments(message) = refused else {
        panic!("an unknown agent name is a bad argument, not a failure: {refused:?}");
    };
    assert!(
        message.contains("no agent named `explorer-2`"),
        "the refusal does not say what was wrong: {message}"
    );
    assert!(
        message.contains("Available: explorer"),
        "the refusal does not say what would work instead: {message}"
    );
}

/// **A delegation is lent half of what is left, and the agent file's own
/// iteration cap.**
///
/// Both numbers are on the `delegation` record, which is the only place a
/// person can read them back. Mutation-checked, one line each: lend the whole
/// remaining allowance rather than half, or take the parent's iteration cap
/// rather than the file's, and this goes red. Neither was visible anywhere
/// before — the suite's parent budget is large enough that a subagent given all
/// of it still finishes, and its iteration cap is never reached either way.
///
/// Half is the number that leaves the parent enough to report what happened
/// after a subagent runs away.
#[tokio::test]
async fn a_delegation_is_lent_half_of_what_is_left_and_the_files_own_iteration_cap() {
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(SessionLog::open(dir.path(), "sess-sub-budget").unwrap());
    let (read, _) = TestTool::returning("Read", "x");
    let mut ty = agent_type("explorer", Some(vec!["Read"]));
    // Distinct from the parent's 20, so "the file's cap" and "the parent's cap"
    // are not the same integer.
    ty.max_turns = Some(7);
    let fake = Fake::new(vec![
        delegate_to("explorer", "look").costing(100),
        text("found it\n\nGOAL COMPLETE"),
        text("ok\n\nGOAL COMPLETE"),
    ]);

    let run = delegating(
        dir.path(),
        fake.clone(),
        &[ty],
        vec![read],
        allowing_everything(),
        budgets(),
        log.clone(),
        Arc::new(Term::silent()),
    )
    .await;
    assert_eq!(run.outcome.ending, Ending::Done);

    let records = SessionLog::read(log.path()).unwrap();
    let record = records
        .iter()
        .find(|r| r["kind"] == "delegation")
        .expect("no delegation was recorded");
    // 100 of the parent's million was gone when the delegation started.
    let remaining = budgets().max_tokens - 100;
    assert_eq!(
        record["max_tokens"].as_i64(),
        Some(remaining / 2),
        "a subagent was lent something other than half of what the goal had \
         left: {record}"
    );
    assert_eq!(
        record["max_iterations"].as_u64(),
        Some(7),
        "the agent file's own iteration cap is not what the sub-run got: {record}"
    );
}

/// **The ending recorded per delegation is the one that happened.**
///
/// `emma agents` counts a run as finished from this field, so a record that
/// always said `done` would report a fleet of subagents that never fails.
/// Mutation-checked: hardcode `"ending": "done"` on the record and this goes
/// red, while `what_a_delegation_cost_can_be_read_back_across_sessions` — whose
/// single delegation really does end `done` — stays green.
///
/// Two delegations in one session, one of each, is what makes that mutation
/// catchable: the `done` half is the control, so hardcoding `"tokens"` instead
/// fails as well.
#[tokio::test]
async fn the_ending_recorded_for_each_delegation_is_the_one_that_happened() {
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(SessionLog::open(dir.path(), "sess-endings").unwrap());
    let (read, _) = TestTool::returning("Read", "x");
    let mut spender = agent_type("spender", Some(vec!["Read"]));
    spender.max_tokens = Some(5_000);
    let fake = Fake::new(vec![
        delegate_to("explorer", "look once"),
        text("found it\n\nGOAL COMPLETE"),
        delegate_to("spender", "read everything"),
        // One call over that type's own cap: the run stops on tokens.
        call("Read", json!({ "file_path": "src/big.rs" })).costing(9_000),
        text("done\n\nGOAL COMPLETE"),
    ]);

    let run = delegating(
        dir.path(),
        fake.clone(),
        &[agent_type("explorer", Some(vec!["Read"])), spender],
        vec![read],
        allowing_everything(),
        budgets(),
        log.clone(),
        Arc::new(Term::silent()),
    )
    .await;
    assert_eq!(run.outcome.ending, Ending::Done);

    let records = SessionLog::read(log.path()).unwrap();
    let endings: Vec<(String, String)> = records
        .iter()
        .filter(|r| r["kind"] == "delegation")
        .map(|r| {
            (
                r["agent"].as_str().unwrap_or_default().to_string(),
                r["ending"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        endings,
        vec![
            ("explorer".to_string(), "done".to_string()),
            ("spender".to_string(), "tokens".to_string()),
        ],
        "the endings on the records are not the endings the two runs had"
    );
}

/// **The brief carries what the caller knows and what it must deliver.**
///
/// A subagent starts with none of the parent's conversation, so `context` and
/// `deliver` are the whole of what it is told beyond the task. Mutation-checked:
/// stop reading either argument in `brief` — one line each — and this goes red.
/// Nothing above passes either field, so both were free to delete.
///
/// The second run is the control, and it is what makes the first meaningful: a
/// `brief` that appended the heading unconditionally would put "what the agent
/// that sent you already knows" over an empty list on every delegation, which
/// is a lie of a smaller size.
#[tokio::test]
async fn the_brief_carries_what_the_caller_knows_and_what_it_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let (read, _) = TestTool::returning("Read", "x");
    let fake = Fake::new(vec![
        call(
            "Delegate",
            json!({
                "agent": "explorer",
                "task": "find every call site of the old constructor",
                "context": ["src/retry.rs is the caller", "cargo build already fails"],
                "deliver": "the paths and line numbers",
            }),
        ),
        text("Two of them.\n\nGOAL COMPLETE"),
        text("Right.\n\nGOAL COMPLETE"),
    ]);
    delegating(
        dir.path(),
        fake.clone(),
        &[agent_type("explorer", Some(vec!["Read"]))],
        vec![read.clone()],
        allowing_everything(),
        budgets(),
        Arc::new(SessionLog::none()),
        Arc::new(Term::silent()),
    )
    .await;

    let seen = fake.transcript();
    assert!(
        seen.contains("- src/retry.rs is the caller")
            && seen.contains("- cargo build already fails"),
        "what the caller already knew never reached the sub-run: {seen}"
    );
    assert!(
        seen.contains("Your answer must contain: the paths and line numbers"),
        "what the caller asked for never reached the sub-run: {seen}"
    );

    // The control: a delegation that passed neither says neither. `context` is
    // present and **empty**, which is the shape that catches a heading written
    // unconditionally — a key that is absent is skipped by the `if let` above
    // whatever the emptiness check does, so a bare `delegate_to` here would
    // leave that guard free to delete.
    let dir = tempfile::tempdir().unwrap();
    let bare = Fake::new(vec![
        call(
            "Delegate",
            json!({
                "agent": "explorer",
                "task": "find every call site of the old constructor",
                "context": [],
            }),
        ),
        text("Two of them.\n\nGOAL COMPLETE"),
        text("Right.\n\nGOAL COMPLETE"),
    ]);
    delegating(
        dir.path(),
        bare.clone(),
        &[agent_type("explorer", Some(vec!["Read"]))],
        vec![read],
        allowing_everything(),
        budgets(),
        Arc::new(SessionLog::none()),
        Arc::new(Term::silent()),
    )
    .await;
    let seen = bare.transcript();
    assert!(
        !seen.contains("already knows"),
        "a delegation with no context announced context anyway: {seen}"
    );
    assert!(
        !seen.contains("Your answer must contain"),
        "a delegation with no `deliver` demanded one anyway: {seen}"
    );
}

/// **A subagent inherits the project's standing instructions, and is told what
/// it is.**
///
/// Three lines, none of them observable in anything a delegation returns, all
/// three in the sub-run's system prompt:
///
/// - the project's own instructions, above the agent file's body. Rules about
///   evidence and honesty are not persona-specific, and a delegated agent that
///   does not inherit *report what the checks actually said* is one that
///   reports what they were expected to say.
/// - the framing that its final message is the only thing that reaches the
///   caller, which is what makes it write one that stands alone.
/// - the caller's `deliver`, restated.
///
/// Mutation-checked, one line each: drop the harness half of `instructions`,
/// drop the `push_str` of the framing, or drop the `deliver` restatement, and
/// this goes red. All three survived the whole suite before it existed.
#[tokio::test]
async fn a_subagent_inherits_the_projects_instructions_and_is_told_who_it_works_for() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".emma");
    std::fs::create_dir_all(root.join("personas").join("tester")).unwrap();
    std::fs::write(root.join("config.json"), "{}").unwrap();
    std::fs::write(
        root.join("personas").join("tester").join("soul.md"),
        "Report what the checks actually said, not what they were expected to say.\n",
    )
    .unwrap();
    let harness =
        Arc::new(Harness::load_selecting(&root, Flavor::Emma, Some("tester".to_string())).unwrap());
    assert!(
        !harness.instructions.is_empty(),
        "the fixture harness carries no standing instructions, so this test \
         could not tell whether they were inherited"
    );

    let (read, _) = TestTool::returning("Read", "x");
    let fake = Fake::new(vec![
        text("the first answer\n\nGOAL COMPLETE"),
        text("the second answer\n\nGOAL COMPLETE"),
    ]);
    let provider = Watching::over(fake.clone());
    let base: Arc<dyn Provider> = provider.clone();
    let (delegate, _) = Delegate::new(
        Nest {
            harness,
            approvals: allowing_everything(),
            log: Arc::new(SessionLog::none()),
            term: Arc::new(Term::silent()),
            interrupt: Interrupt::new(),
            spend: Spend::new(),
            cwd: dir.path().to_path_buf(),
            session_id: "sess-test".into(),
            caching: Caching::On,
            budgets: budgets(),
            running: Running::new(base),
        },
        &[agent_type("explorer", Some(vec!["Read"]))],
        &[read],
        None,
        &|_| None,
    );
    let delegate = delegate.expect("no agent types resolved");
    let ctx = ToolCtx {
        cwd: dir.path().to_path_buf(),
        session_id: "sess-test".into(),
        turn_id: "turn-1".into(),
        background: Default::default(),
    };

    let out = delegate
        .invoke(
            &ctx,
            json!({
                "agent": "explorer",
                "task": "go and look",
                "deliver": "the failing assertion, verbatim",
            }),
        )
        .await
        .unwrap();
    assert!(out.is_ok(), "{out:?}");

    assert_eq!(
        provider.modes(),
        vec![Mode::Batch],
        "a sub-run was asked for streaming: it reports once at the end, and a \
         subordinate terminal swallows the prose either way"
    );

    let prompts = provider.prompts();
    let prompt = prompts.first().expect("the sub-run was never called");
    assert!(
        prompt.contains("Report what the checks actually said"),
        "the sub-run did not inherit the project's standing instructions: {prompt}"
    );
    assert!(
        prompt.contains("You are explorer."),
        "the agent file's own body is missing from its prompt: {prompt}"
    );
    assert!(
        prompt.contains("working on behalf of another agent"),
        "the sub-run was never told its final message is all that reaches the \
         caller: {prompt}"
    );
    assert!(
        prompt.contains("asked specifically for: the failing assertion, verbatim"),
        "the caller's `deliver` was not restated to the agent that has to \
         satisfy it: {prompt}"
    );

    // The controls, both about those two lines being conditional rather than
    // boilerplate that would be there whatever was passed.
    let out = delegate
        .invoke(&ctx, json!({ "agent": "explorer", "task": "go and look" }))
        .await
        .unwrap();
    assert!(out.is_ok(), "{out:?}");
    let second = provider.prompts()[1].clone();
    assert!(
        !second.contains("asked specifically for"),
        "a delegation with no `deliver` restated one anyway: {second}"
    );
    assert!(
        !MarkerClaim.contract().contains("working on behalf"),
        "the framing is in the contract every goal already runs under, so it \
         says nothing about being a subagent"
    );
}

// endregion: What the sub-run is actually given
