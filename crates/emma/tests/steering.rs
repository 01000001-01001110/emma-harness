//! Steering: what a person types while a goal is already running.
//!
//! Two claims are under test here and they pull in opposite directions, which
//! is why they share a file.
//!
//! **The injection works.** A line typed during iteration 1 is in front of the
//! model on iteration 2, attributed, inside the user turn that was already
//! there rather than as a second one after it, and the session file folds back
//! to the conversation that was actually sent.
//!
//! **The gate is not weakened.** Nothing typed while an approval question is
//! pending can reach the steering queue, and nothing in the steering queue can
//! answer an approval question. `approval.rs` drains the line channel before
//! every prompt for a reason that predates this feature, and a steering queue
//! that could be reached from either side would be that drain undone.

mod support;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use emma::agent::{Agent, Budgets, Ending, Interrupt, Outcome, Setup};
use emma::approval::{Answer, Approvals, Asker, Gate};
use emma::goal::{Goal, MarkerClaim};
use emma::session::{self, SessionLog};
use emma::session_command::{mid_goal, MidGoal};
use emma::steering::{Steer, Steering};
use emma::term::Term;
use emma_harness::{Flavor, Harness};
use emma_llm::{Caching, Message, Mode, Role};
use emma_tool_api::{NetworkTarget, Registry, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use support::{call, empty_harness, registry, text, Fake, Say};

fn budgets() -> Budgets {
    Budgets {
        max_iterations: 20,
        max_tokens: 1_000_000,
        wall_clock: Duration::from_secs(60),
        max_kicks: 3,
        max_context: 1_000_000,
    }
}

/// A tool that types for the user.
///
/// The steer has to arrive **between** two model calls, which is the only
/// moment the seam exists at, and a tool call is the one thing a scripted run
/// can hang that on: it runs inside iteration 1, after the model answered and
/// before the loop comes back round. A background thread racing the loop would
/// test the scheduler rather than the drain.
struct TypesWhileWorking {
    queue: Steering,
    steer: Steer,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for TypesWhileWorking {
    fn name(&self) -> &'static str {
        "Work"
    }

    fn description(&self) -> &str {
        "does nothing, and the user types while it happens"
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "x": { "type": "string" } } })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: true,
            reaches_network: false,
            idempotent: true,
        }
    }

    fn network_target(&self, _args: &Value) -> Option<NetworkTarget> {
        None
    }

    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        _args: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        // Once. A second call would queue a second steer and the test would be
        // asserting about a queue it did not set up.
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.queue.push(self.steer.clone());
        }
        Ok(Ok(ToolOutcome::new("Work ran")))
    }
}

/// One goal, one agent, one steering queue. Returns the outcome and the
/// conversation the agent is left holding.
#[allow(clippy::too_many_arguments)]
async fn drive(
    root: &Path,
    cwd: &Path,
    tools: Registry,
    approvals: &Approvals,
    provider: &Arc<Fake>,
    goal: &Goal,
    log: &SessionLog,
    steering: Steering,
) -> (Outcome, Vec<Message>) {
    let harness = Harness::load_selecting(root, Flavor::Emma, None).unwrap();
    let term = Term::silent();
    let mut agent = Agent::new(Setup {
        background: Default::default(),
        provider: provider.clone(),
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals,
        log,
        term: &term,
        interrupt: Interrupt::new(),
        spend: emma::agent::Spend::new(),
        done: &MarkerClaim,
        cwd: cwd.to_path_buf(),
        session_id: "sess-test".into(),
        budgets: budgets(),
        caching: Caching::On,
        sampling: Default::default(),
        mode: Mode::Batch,
        web_search: false,
    })
    .steered_by(steering, None);
    let outcome = agent.run_goal(goal).await;
    (outcome, agent.conversation())
}

// region: The injection
// ---------------------------------------------------------------------------
// The injection
//
// Red-then-green on the seam itself: with `drain_the_steering_queue` removed
// from the top of the loop, the first two assertions below fail: the sentence
// is nowhere in the second request. With the drain in and
// `session::append_user_text` replaced by a `push`, the roles-alternate
// assertion fails instead, which is the 400 the API would have answered with.
// ---------------------------------------------------------------------------

/// The whole feature, in one run.
#[tokio::test]
async fn a_steer_typed_during_iteration_one_is_in_iteration_twos_request() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let queue = Steering::new();
    let tool = Arc::new(TypesWhileWorking {
        queue: queue.clone(),
        steer: Steer::Text("stop editing and explain what you found".into()),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let provider = Fake::new(vec![call("Work", json!({})), text("GOAL COMPLETE")]);
    let log = SessionLog::open(dir.path(), "sess-0000000000001-1").unwrap();

    let (outcome, conversation) = drive(
        &root,
        dir.path(),
        registry(vec![tool]),
        &Approvals::unattended(),
        &provider,
        &Goal::new("make it work"),
        &log,
        queue.clone(),
    )
    .await;
    assert_eq!(outcome.ending, Ending::Done);
    assert!(queue.is_empty(), "the queue was not drained");

    // The claim: the model read it, on the call after it was typed.
    let sent = provider.last_messages();
    let rendered = sent
        .iter()
        .map(|m| format!("{:?}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("stop editing and explain what you found"),
        "the steer never reached the model:\n{rendered}"
    );
    // Attributed. Without this the model reads a mid-task correction as part of
    // the tool output it is appended to.
    assert!(
        rendered.contains("while you were working"),
        "the steer is not marked as the person speaking:\n{rendered}"
    );

    // The shape. Two user turns in a row is a 400 rather than a conversation,
    // so the steer rides the turn that was already there.
    for pair in sent.windows(2) {
        assert_ne!(
            pair[0].role, pair[1].role,
            "two turns of the same role in a row:\n{rendered}"
        );
    }
    // And it rides the *tool results*, which is the user turn the loop had
    // just placed. Iteration 2's request therefore carries one user turn
    // holding the results and the sentence, not two.
    let last = sent.last().expect("a request was sent");
    assert_eq!(last.role, Role::User);
    let text = last.content.to_string();
    assert!(
        text.contains("Work ran") && text.contains("stop editing"),
        "the steer did not join the tool results: {text}"
    );

    // The fold. A resume must replay the conversation that was sent, and the
    // steer is part of it.
    let restored = session::restore(&log.path()).unwrap();
    let folded = restored.resumed.messages;
    assert_eq!(
        folded, conversation,
        "the folded session is not the conversation that ran"
    );
    assert!(
        folded
            .iter()
            .any(|m| m.content.to_string().contains("stop editing")),
        "the steer is not in the folded session: {folded:#?}"
    );
}

/// Several lines typed in one turn are one interjection, in the order they were
/// typed, and they arrive together rather than one per turn.
#[tokio::test]
async fn several_steers_arrive_together_in_the_order_they_were_typed() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let queue = Steering::new();
    queue.push(Steer::Text("use tokio".into()));
    queue.push(Steer::Text("and skip the docs".into()));
    let provider = Fake::new(vec![text("GOAL COMPLETE")]);

    // Queued before the goal opens, so the first iteration's drain takes both.
    let (_, conversation) = drive(
        &root,
        dir.path(),
        registry(vec![]),
        &Approvals::unattended(),
        &provider,
        &Goal::new("make it work"),
        &SessionLog::none(),
        queue,
    )
    .await;

    let opening = conversation
        .iter()
        .find(|m| m.role == Role::User)
        .expect("the goal opened")
        .content
        .to_string();
    let first = opening.find("use tokio").expect("first steer missing");
    let second = opening.find("and skip the docs").expect("second missing");
    assert!(first < second, "the order was not kept: {opening}");
    // One attribution, not one per line: this is a person talking, not two
    // interruptions.
    assert_eq!(opening.matches("while you were working").count(), 1);
    // The goal is still the goal. A steer joins the opening turn; it does not
    // replace it.
    assert!(opening.contains("make it work"), "{opening}");
}

/// A boundary command runs at the drain point, exactly as if it had been typed
/// between goals, and its receipt reaches the transcript.
#[tokio::test]
async fn a_boundary_command_applies_at_the_turn_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let queue = Steering::new();
    let tool = Arc::new(TypesWhileWorking {
        queue: queue.clone(),
        // `/mode plan` writes nothing to disk, which keeps this test off the
        // developer's real settings file. It is also the arm worth proving:
        // the posture is what the approval gate reads.
        steer: Steer::Command(Box::new(emma::SessionCommand::Mode(Some("plan".into())))),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let provider = Fake::new(vec![call("Work", json!({})), text("GOAL COMPLETE")]);
    let approvals = Approvals::unattended();
    assert_eq!(approvals.mode(), emma::Mode::Assist);

    let term = Term::recording();
    let harness = Harness::load_selecting(&root, Flavor::Emma, None).unwrap();
    let tools = registry(vec![tool]);
    let log = SessionLog::none();
    let mut agent = Agent::new(Setup {
        background: Default::default(),
        provider: provider.clone(),
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals: &approvals,
        log: &log,
        term: &term,
        interrupt: Interrupt::new(),
        spend: emma::agent::Spend::new(),
        done: &MarkerClaim,
        cwd: dir.path().to_path_buf(),
        session_id: "sess-test".into(),
        budgets: budgets(),
        caching: Caching::On,
        sampling: Default::default(),
        mode: Mode::Batch,
        web_search: false,
    })
    .steered_by(queue, None);
    agent.run_goal(&Goal::new("make it work")).await;

    // The posture switched, mid-goal, at the boundary.
    assert_eq!(approvals.mode(), emma::Mode::Plan);
    // And it said so. A command that applied silently is the half-applied
    // command in a different costume.
    assert!(
        term.recorded().iter().any(|s| s.contains("mode      plan")),
        "no receipt: {:#?}",
        term.recorded()
    );
    // A command is not a sentence: nothing about `/mode` reaches the model.
    assert!(
        !provider.transcript().contains("/mode"),
        "a built-in was handed to the model as text: {}",
        provider.transcript()
    );
}

// endregion: The injection

// region: The poisoning invariant
// ---------------------------------------------------------------------------
// The poisoning invariant
//
// The law this feature was not allowed to break, from both directions. See
// `approval.rs`, whose drain exists because a `y` typed before a prompt existed
// once became a goal.
// ---------------------------------------------------------------------------

/// **Direction one: nothing typed under a question can enter the queue.**
///
/// The routing decision is where this is settled, on the thread that owns the
/// keyboard, and it is settled before the line goes anywhere, so an answer
/// cannot be queued even for an instant.
#[test]
fn a_line_typed_while_a_question_is_pending_never_reaches_the_queue() {
    for line in ["y", "n", "a", "yes", "keep going, but use tokio", "/models"] {
        assert_eq!(
            mid_goal(line, true, true),
            MidGoal::Send,
            "`{line}` typed under an approval prompt must go to the answer path"
        );
    }
    // The control: the same lines with no question on screen are steering. If
    // this half stopped being true the assertion above would pass for the
    // wrong reason, because nothing would ever be queued at all.
    for line in ["keep going, but use tokio", "/models"] {
        assert_eq!(mid_goal(line, true, false), MidGoal::Queue, "{line}");
    }
}

/// **Direction two: nothing in the queue can answer a question.**
///
/// Structural rather than careful: `Approvals::ask` reads `LineSource` and the
/// steering queue is not one, so a steer has no path to a prompt. The test
/// stands the two beside each other, a queue with an emphatic `y` in it and an
/// approval whose scripted answer is `No`, and proves the gate refused.
#[tokio::test]
async fn a_queued_steer_never_answers_a_later_approval() {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let queue = Steering::new();
    let tool = Arc::new(TypesWhileWorking {
        queue: queue.clone(),
        // The exact byte the drain law exists for.
        steer: Steer::Text("y".into()),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    // One `No`, and nothing else. If the queued `y` could reach the gate the
    // second call would be allowed on an answer nobody gave.
    let approvals = Approvals::new(Gate::Ask, Asker::Scripted(vec![Answer::No].into()));
    // One turn, two calls: `Work` queues the `y` and `Write` asks the gate
    // immediately afterwards, so the question is asked with the `y` sitting in
    // the steering queue. That is the moment the invariant is about: a queue
    // that had already been drained would prove nothing.
    let provider = Fake::new(vec![
        Say {
            text: String::new(),
            calls: vec![
                ("Work".into(), json!({})),
                ("Write".into(), json!({ "path": "a" })),
            ],
            tokens: 10,
            truncated: false,
            paused: false,
            fail: None,
        },
        call("Write", json!({ "path": "b" })),
        text("GOAL COMPLETE"),
    ]);
    let tools = registry(vec![tool.clone(), writing_tool()]);

    let (_, conversation) = drive(
        &root,
        dir.path(),
        tools,
        &approvals,
        &provider,
        &Goal::new("make it work"),
        &SessionLog::none(),
        queue.clone(),
    )
    .await;

    // The queue was drained as steering, so the `y` reached the model as the
    // person's words rather than the gate as an answer. That is the whole
    // separation: one channel per meaning.
    let rendered = conversation
        .iter()
        .map(|m| m.content.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("while you were working"),
        "the steer did not travel as steering: {rendered}"
    );
    // Both writes were refused. The scripted asker gave one `No` and then ran
    // out, and running out is a denial too. Neither call was allowed, and
    // nothing in the steering queue could make one of them a `Yes`.
    assert_eq!(
        rendered.matches("declined").count() + rendered.matches("No approval").count(),
        2,
        "a write got through the gate: {rendered}"
    );
}

/// A tool that writes, so the gate has something to ask about.
fn writing_tool() -> Arc<dyn Tool> {
    struct Writes;
    #[async_trait::async_trait]
    impl Tool for Writes {
        fn name(&self) -> &'static str {
            "Write"
        }
        fn description(&self) -> &str {
            "writes, so the gate has to ask"
        }
        fn input_schema(&self) -> Value {
            json!({ "type": "object", "properties": { "path": { "type": "string" } } })
        }
        fn meta(&self) -> ToolMeta {
            ToolMeta {
                read_only: false,
                reaches_network: false,
                idempotent: false,
            }
        }
        fn network_target(&self, _args: &Value) -> Option<NetworkTarget> {
            None
        }
        async fn invoke(
            &self,
            _ctx: &ToolCtx,
            _args: Value,
        ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
            Ok(Ok(ToolOutcome::new("Write ran")))
        }
    }
    Arc::new(Writes)
}

// endregion: The poisoning invariant
