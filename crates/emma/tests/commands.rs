//! The session's own commands, driven through the same seam `main` uses.
//!
//! Everything here goes through `session_command::parse` and, where it can,
//! `session_command::run` — the two functions `main`'s loop calls and nothing
//! else. That is deliberate: the point of moving the dispatch out of `main.rs`
//! was that a command's behaviour stops being something only a person at a
//! terminal can check.
//!
//! What still cannot be reached from here is the mpsc between the reader thread
//! and the loop, and the `break` itself. `the_way_out_of_the_session` below goes
//! as far as a test can — the menu's own string, through the parser, to
//! `Flow::Exit` — and the report says what is left.

mod support;

use std::path::Path;
use std::sync::Arc;

use emma::agent::{Agent, Budgets, Interrupt, Running, Setup, Spend};
use emma::approval::{Answer, Approvals, Asker, Gate};
use emma::goal::{Goal, MarkerClaim};
use emma::session::SessionLog;
use emma::session_command::{self, Flow, Session};
use emma::term::Term;
use emma_harness::{Flavor, Harness};
use emma_llm::{auth::ApiKey, Caching, Message, Mode, Provider};
use emma_tool_api::Registry;
use serde_json::Value;
use support::{call, empty_harness, registry, rejected, text, Fake, TestTool};

// region: The fixture
// ---------------------------------------------------------------------------
// The fixture
//
// A real `Agent` over a scripted provider, a real harness in a temporary
// directory, and a real session file — because the two properties worth
// asserting here (`/clear` survives a resume, `/compact` is replayed rather
// than re-decided) are both about what the fold reads back out of that file.
// ---------------------------------------------------------------------------

struct Fixture {
    dir: tempfile::TempDir,
    harness: Harness,
    tools: Registry,
    approvals: Approvals,
    term: Term,
    log: SessionLog,
    kind: &'static dyn emma_llm::ProviderKind,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let harness = Harness::load_selecting(&root, Flavor::Emma, None).unwrap();
    let (tool, _) = TestTool::ok("Fine", true);
    Fixture {
        harness,
        tools: registry(vec![tool]),
        approvals: Approvals::unattended(),
        term: Term::recording(),
        log: SessionLog::open(dir.path(), "sess-commands").unwrap(),
        kind: emma_llm::kind("anthropic").unwrap(),
        dir,
    }
}

/// The same machine with a gate that asks, one writing tool, and the answers
/// already queued.
///
/// It exists for `/clear`'s receipt. The default fixture is `unattended` with a
/// read-only tool, so no question is ever put and no session grant can exist —
/// which means the "kept" line has only ever been read in its empty form.
fn fixture_asking(answers: Vec<Answer>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = empty_harness(dir.path());
    let harness = Harness::load_selecting(&root, Flavor::Emma, None).unwrap();
    // Not read-only, so the gate actually asks rather than exempting the call.
    let (tool, _) = TestTool::ok("Edit", false);
    Fixture {
        harness,
        tools: registry(vec![tool]),
        approvals: Approvals::new(Gate::Ask, Asker::Scripted(answers.into())),
        term: Term::recording(),
        log: SessionLog::open(dir.path(), "sess-commands").unwrap(),
        kind: emma_llm::kind("anthropic").unwrap(),
        dir,
    }
}

impl Fixture {
    fn agent<'a>(&'a self, provider: Arc<dyn Provider>) -> Agent<'a> {
        self.agent_with(provider, Budgets::default())
    }

    /// The same agent with budgets the caller chose — used to drive the
    /// *automatic* compaction path, which only runs when a request exceeds
    /// `max_context`.
    fn agent_with<'a>(&'a self, provider: Arc<dyn Provider>, budgets: Budgets) -> Agent<'a> {
        Agent::new(Setup {
            background: Default::default(),
            provider,
            harness: &self.harness,
            instructions: &self.harness.instructions,
            tools: &self.tools,
            approvals: &self.approvals,
            log: &self.log,
            term: &self.term,
            interrupt: Interrupt::new(),
            spend: Spend::new(),
            done: &MarkerClaim,
            cwd: self.dir.path().to_path_buf(),
            session_id: "sess-commands".into(),
            budgets,
            caching: Caching::On,
            mode: Mode::Batch,
            web_search: false,
        })
    }

    /// Run one command exactly as `main` does.
    async fn command<'a>(
        &'a self,
        agent: &mut Agent<'a>,
        provider: &mut Arc<dyn Provider>,
        running: &Running,
        line: &str,
    ) -> Flow {
        let cmd = session_command::parse(line)
            .unwrap_or_else(|| panic!("`{line}` is not a session command"));
        let mut session = Session {
            agent,
            term: &self.term,
            approvals: &self.approvals,
            harness: &self.harness,
            tools: &self.tools,
            cwd: self.dir.path(),
            session_dir: Some(self.dir.path()),
            unavailable: &[],
            provider,
            running,
            kind: self.kind,
            key: ApiKey::new("sk-ant-not-a-real-key"),
            log_path: self.log.path().to_path_buf(),
            home: Some(self.dir.path().to_path_buf()),
            theme_at_start: None,
        };
        session_command::run(cmd, &mut session).await
    }

    fn said(&self) -> String {
        self.term.recorded().join("\n")
    }
}

/// A goal that reads a file and answers, which is two messages of tool traffic
/// per goal — enough for compaction to have something to take out.
fn working_goal() -> Vec<support::Say> {
    vec![
        call("Fine", serde_json::json!({ "x": "1" })),
        text("done\n\nGOAL COMPLETE"),
    ]
}

fn folded(log: &SessionLog) -> Vec<Message> {
    emma::session::fold(log.path()).unwrap()
}

// endregion: The fixture

// region: The way out
// ---------------------------------------------------------------------------
// The way out
//
// `/exit` and `/quit` were a `matches!` in front of the parser and had no test
// at all — the one command whose failure ends nothing and whose dispatch
// nothing checked. They are ordinary members of the command set now, and this
// is the assertion that was missing.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_way_out_of_the_session_works_by_both_routes() {
    let f = fixture();
    let provider = Fake::new(Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    // Route one: typed at the prompt, in every spelling a person uses.
    for line in ["/exit", "/quit", "  /exit  ", "/EXIT"] {
        assert_eq!(
            f.command(&mut agent, &mut current, &running, line).await,
            Flow::Exit,
            "`{line}` did not end the session"
        );
    }

    // Route two: picked from the `/` menu, which builds the string itself. This
    // is the half a reader cannot check by looking at `main.rs`, because the
    // string never appears there — `MenuKey::Accept` composes it from the
    // highlighted row and sends it down the same channel a typed line uses.
    let mut menu = emma::term::menu::Menu::for_project(&["review"]);
    for typed in ["/", "/e", "/exit", "/q"] {
        menu.sync(typed, false);
        let name = menu
            .selection()
            .map(|e| format!("/{}", e.name))
            .unwrap_or_else(|| panic!("nothing highlighted for `{typed}`"));
        if !name.starts_with("/e") && !name.starts_with("/q") {
            continue;
        }
        assert_eq!(
            f.command(&mut agent, &mut current, &running, &name).await,
            Flow::Exit,
            "the menu sent `{name}` and it did not end the session"
        );
    }
}

// endregion: The way out

// region: /clear
// ---------------------------------------------------------------------------
// /clear
//
// The prerequisite the design calls hard: `--resume` folds the whole session
// file, so without the `cleared` arm a resume puts the cleared conversation
// straight back. The equality below is the property the fold is specified by,
// and it is the one assertion that catches a wrong arm.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn clear_survives_a_resume_because_the_fold_honours_it() {
    let f = fixture();
    let mut script = working_goal();
    script.extend(working_goal());
    script.extend(working_goal());
    let provider = Fake::new(script);
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    agent.run_goal(&Goal::new("first")).await;
    agent.run_goal(&Goal::new("second")).await;
    let before = agent.conversation();
    assert!(before.len() >= 4, "{before:?}");

    f.command(&mut agent, &mut current, &running, "/clear")
        .await;
    assert!(
        agent.conversation().is_empty(),
        "the conversation survived /clear"
    );

    agent.run_goal(&Goal::new("third")).await;
    // The whole point: fold the file a `--resume` would read, and it must equal
    // what the agent is actually holding. If the `cleared` arm were missing,
    // this list would still contain the two cleared goals.
    assert_eq!(folded(&f.log), agent.conversation());
    let folded_text = format!("{:?}", folded(&f.log));
    assert!(
        !folded_text.contains("first") && !folded_text.contains("second"),
        "a cleared goal came back through the fold: {folded_text}"
    );
    assert!(folded_text.contains("third"), "{folded_text}");
}

#[tokio::test]
async fn clear_says_what_it_kept_rather_than_leaving_it_to_be_discovered() {
    let f = fixture();
    let provider = Fake::new(working_goal());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    agent.run_goal(&Goal::new("first")).await;

    f.command(&mut agent, &mut current, &running, "/clear")
        .await;
    let said = f.said();
    // The receipt is the whole of the safety argument: the grants are kept on
    // purpose, and silence about them is the trap. It must also name the
    // transcript, because "did I just lose my log?" is the next question.
    assert!(said.contains("cleared"), "{said}");
    assert!(said.contains("grant"), "{said}");
    assert!(said.contains("sess-commands"), "{said}");
}

/// The half of `/clear` that is easy to forget: a `--resume`d session has a
/// conversation waiting in `Agent::resumed` that has not reached the chapters
/// yet, and it is injected into the *next* goal's query. Clearing the chapters
/// and leaving that behind would clear everything except the thing the user was
/// most obviously trying to get rid of.
#[tokio::test]
async fn clear_before_the_first_goal_of_a_resumed_session_drops_the_restored_turns() {
    let f = fixture();
    let provider = Fake::new(working_goal());
    let mut agent = f.agent(provider.clone()).resuming(emma::agent::Resumed {
        messages: vec![Message::user("the conversation being resumed")],
        ..Default::default()
    });
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    f.command(&mut agent, &mut current, &running, "/clear")
        .await;
    agent.run_goal(&Goal::new("after the clear")).await;

    let sent = format!("{:?}", provider.last_messages());
    assert!(
        !sent.contains("the conversation being resumed"),
        "the restored conversation survived /clear: {sent}"
    );
}

// endregion: /clear

// region: /compact
// ---------------------------------------------------------------------------
// /compact
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compact_is_replayed_by_the_fold_rather_than_re_decided() {
    let f = fixture();
    let mut script = working_goal();
    script.extend(working_goal());
    script.extend(working_goal());
    script.extend(working_goal());
    let provider = Fake::new(script);
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    agent.run_goal(&Goal::new("first")).await;
    agent.run_goal(&Goal::new("second")).await;
    agent.run_goal(&Goal::new("third")).await;
    let before = agent.conversation().len();

    f.command(&mut agent, &mut current, &running, "/compact")
        .await;
    let after = agent.conversation();
    assert!(
        after.len() < before,
        "nothing was compacted: {before} -> {}",
        after.len()
    );
    // The last goal is kept, which is the rule `/compact` is worded by.
    let text = format!("{after:?}");
    assert!(
        text.contains("third"),
        "the last goal was compacted: {text}"
    );

    agent.run_goal(&Goal::new("fourth")).await;
    assert_eq!(
        folded(&f.log),
        agent.conversation(),
        "the fold re-derived a compaction instead of replaying the record"
    );
}

#[tokio::test]
async fn compact_all_includes_the_goal_that_just_finished() {
    let f = fixture();
    let mut script = working_goal();
    script.extend(working_goal());
    let provider = Fake::new(script);
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    agent.run_goal(&Goal::new("first")).await;
    agent.run_goal(&Goal::new("second")).await;

    f.command(&mut agent, &mut current, &running, "/compact all")
        .await;
    let text = format!("{:?}", agent.conversation());
    assert!(
        !text.contains("Fine ran"),
        "tool traffic survived /compact all: {text}"
    );
}

#[tokio::test]
async fn compact_with_an_instruction_says_it_cannot_follow_one() {
    let f = fixture();
    let provider = Fake::new(working_goal());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    agent.run_goal(&Goal::new("first")).await;
    let before = agent.conversation();

    f.command(
        &mut agent,
        &mut current,
        &running,
        "/compact keep the API details",
    )
    .await;
    let said = f.said();
    // Quoted back, so the user can see which words were not acted on. Ignoring
    // them silently is the failure this is written against.
    assert!(said.contains("keep the API details"), "{said}");
    assert!(said.contains("does not call a model"), "{said}");
    // …and nothing happened, because a half-honoured instruction is worse than
    // a refused one.
    assert_eq!(agent.conversation(), before);
}

/// Found on the first live run, not by a test: `/compact all` over two
/// four-word goals reported "the conversation is already summarised", which was
/// false. The arithmetic was right — `COMPACTED_NOTE` is longer than the goals
/// it would have replaced — and the sentence describing it was about a
/// different situation entirely.
#[tokio::test]
async fn a_compaction_that_would_not_shrink_says_that_rather_than_claiming_it_already_happened() {
    let f = fixture();
    let mut script = vec![text("one\n\nGOAL COMPLETE")];
    script.push(text("two\n\nGOAL COMPLETE"));
    let provider = Fake::new(script);
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    agent.run_goal(&Goal::new("say one")).await;
    agent.run_goal(&Goal::new("say two")).await;

    f.command(&mut agent, &mut current, &running, "/compact all")
        .await;
    let said = f.said();
    assert!(
        said.contains("would not make the conversation smaller"),
        "{said}"
    );
    assert!(
        !said.contains("already summarised"),
        "it claimed a compaction that never happened: {said}"
    );
}

#[tokio::test]
async fn compact_with_nothing_to_do_says_so_rather_than_nothing() {
    let f = fixture();
    let provider = Fake::new(Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    f.command(&mut agent, &mut current, &running, "/compact")
        .await;
    assert!(f.said().contains("nothing to compact"), "{}", f.said());
}

// endregion: /compact

// region: /model
// ---------------------------------------------------------------------------
// /model
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_model_change_keeps_the_conversation_and_the_next_call_goes_to_the_new_client() {
    let f = fixture();
    let first = Fake::named("model-one", working_goal());
    let second = Fake::named("model-two", working_goal());
    let mut agent = f.agent(first.clone());
    agent.run_goal(&Goal::new("first")).await;
    let carried = agent.conversation();
    assert!(!carried.is_empty());

    let was = agent.set_provider(second.clone());
    assert_eq!(was, "model-one");
    assert_eq!(agent.conversation(), carried, "/model dropped the history");

    agent.run_goal(&Goal::new("second")).await;
    // The second client did the work, and it was sent the first goal's tool
    // traffic — which is what "the conversation survives a model change" means
    // in the only place it can be observed.
    assert_eq!(second.calls(), 2, "the new client was not used");
    assert_eq!(first.calls(), 2, "the old client was called again");
    assert!(
        second.last_query().contains("second") || second.transcript().contains("Fine ran"),
        "{}",
        second.transcript()
    );
}

#[tokio::test]
async fn an_unusable_model_id_is_refused_with_nothing_half_applied() {
    let f = fixture();
    let provider = Fake::named("model-one", Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    f.command(&mut agent, &mut current, &running, "/model src/lib.rs")
        .await;

    // Every one of the four things `/model` touches is untouched. A command
    // that swapped the provider and then failed to update the status row would
    // leave the session lying about which model was running.
    assert_eq!(current.model_id(), "model-one");
    assert_eq!(running.get().model_id(), "model-one");
    assert!(f.said().contains("nothing was changed"), "{}", f.said());
    assert!(
        !Path::new(&f.dir.path().join(".emma").join("settings.json")).exists(),
        "a refused /model wrote settings.json"
    );
}

#[tokio::test]
async fn model_with_no_argument_reports_what_is_running_and_what_it_accepts() {
    let f = fixture();
    let provider = Fake::named("claude-opus-5", Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    f.command(&mut agent, &mut current, &running, "/model")
        .await;
    let said = f.said();
    assert!(said.contains("claude-opus-5"), "{said}");
    // The clamp Emma applies silently on every request, said out loud.
    assert!(said.contains("max_tokens"), "{said}");
    assert!(said.contains("128000"), "{said}");
}

/// The recovery for the one thing that can turn `/model` into a 400: signed
/// thinking blocks minted by the model the session has just left.
///
/// Whether Anthropic actually rejects them is **not established** — see the
/// report. What is established here is that if it does, the session recovers
/// rather than ending, and that it recovers exactly once.
#[tokio::test]
async fn a_rejected_signature_after_a_model_change_is_compacted_and_retried_once() {
    let f = fixture();
    let first = Fake::named("model-one", working_goal());
    let mut agent = f.agent(first.clone());
    agent.run_goal(&Goal::new("first")).await;
    // The fake puts a signed thinking block on every turn, so the conversation
    // now holds exactly the content this recovery is about.
    let held = format!("{:?}", agent.conversation());
    assert!(held.contains("Thinking"), "{held}");

    let second = Fake::named(
        "model-two",
        vec![
            rejected("messages.0.content.1.signature: invalid signature"),
            text("recovered\n\nGOAL COMPLETE"),
        ],
    );
    agent.set_provider(second.clone());
    let outcome = agent.run_goal(&Goal::new("second")).await;

    assert_eq!(outcome.ending, emma::agent::Ending::Done, "{outcome:?}");
    assert_eq!(second.calls(), 2, "it did not retry, or retried twice");
    // What made the retry acceptable: the summaries carry no signed content.
    let sent = second.last_messages();
    let sent_text = format!("{sent:?}");
    assert!(
        !sent_text.contains("signature"),
        "the retry re-sent signed content: {sent_text}"
    );
    assert!(f.said().contains("summarised"), "{}", f.said());
}

/// The recovery must not fire when no model has changed.
///
/// A provider can reject a signature for reasons that have nothing to do with
/// `/model` — a corrupted history, a bug here, a change at the other end. In
/// every one of those, summarising the conversation destroys the tool traffic
/// and fixes nothing, and the user is left with a shorter conversation and the
/// same error one call later.
#[tokio::test]
async fn a_signature_rejection_with_no_model_change_is_reported_not_compacted() {
    let f = fixture();
    let mut script = working_goal();
    script.push(rejected(
        "messages.0.content.1.signature: invalid signature",
    ));
    let provider = Fake::named("model-one", script);
    let mut agent = f.agent(provider.clone());
    agent.run_goal(&Goal::new("first")).await;
    let before = agent.conversation();

    let outcome = agent.run_goal(&Goal::new("second")).await;
    assert!(
        matches!(outcome.ending, emma::agent::Ending::Provider(_)),
        "{outcome:?}"
    );
    let after = format!("{:?}", agent.conversation());
    assert!(
        after.contains("Fine ran"),
        "the first goal's tool traffic was summarised away: {after}"
    );
    assert!(!f.said().contains("summarised"), "{}", f.said());
    assert!(before.len() <= agent.conversation().len());
}

#[tokio::test]
async fn an_ordinary_bad_request_is_still_reported_rather_than_compacted_away() {
    // The guard that stops the recovery eating a conversation for an unrelated
    // 400: it fires only after a model change, and only on a message naming a
    // signature or a thinking block.
    let f = fixture();
    let first = Fake::named("model-one", working_goal());
    let mut agent = f.agent(first.clone());
    agent.run_goal(&Goal::new("first")).await;
    let before = agent.conversation();

    let second = Fake::named(
        "model-two",
        vec![rejected("max_tokens: must be less than 64000")],
    );
    agent.set_provider(second.clone());
    let outcome = agent.run_goal(&Goal::new("second")).await;

    assert!(
        matches!(outcome.ending, emma::agent::Ending::Provider(_)),
        "{outcome:?}"
    );
    assert_eq!(second.calls(), 1, "it retried a 400 that was not about us");
    // The first goal's traffic is still there — nothing was summarised.
    let after = format!("{:?}", agent.conversation());
    assert!(after.contains("Fine ran"), "{after}");
    assert!(before.len() <= agent.conversation().len());

    // …and the near-miss is said out loud. The recovery keys on the provider's
    // prose because no structured code distinguishes the stale-signature case,
    // so a wording change upstream would disable it silently — and the symptom
    // would be a session dying on the first call after `/model` with a message
    // nobody connects to the switch. Both facts that matter are true here: the
    // model changed, and the provider refused.
    let said = f.said();
    assert!(
        said.contains("does not read like the stale-signature case"),
        "a refusal right after a model change passed without a word: {said}"
    );
}

// endregion: /model

// region: What a command must not do
// ---------------------------------------------------------------------------
// What a command must not do
//
// None of these is a tool, and none of them appends to the system prompt. The
// model never learns they exist — the same property `Harness::expand_command`
// already states about project commands, one level up.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_session_command_changes_what_the_model_is_told() {
    let f = fixture();
    let mut script = working_goal();
    script.extend(working_goal());
    let provider = Fake::named("claude-opus-5", script);
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    agent.run_goal(&Goal::new("first")).await;
    let before = goal_hashes(f.log.path());

    for line in ["/help", "/config", "/agents", "/model", "/resume"] {
        f.command(&mut agent, &mut current, &running, line).await;
    }
    agent.run_goal(&Goal::new("second")).await;

    let after = goal_hashes(f.log.path());
    assert_eq!(after.len(), 2, "{after:?}");
    assert_eq!(
        before[0], after[1],
        "a session command changed the instructions or the tool schema"
    );
}

/// `(instructions_hash, tool_schema_hash)` off every `goal` record — both are
/// already written there, so this is an assertion on the log rather than new
/// instrumentation.
fn goal_hashes(path: &Path) -> Vec<(String, String)> {
    SessionLog::read(path)
        .unwrap()
        .into_iter()
        .filter(|r| r["kind"] == "goal")
        .map(|r| {
            (
                r["instructions_hash"].as_str().unwrap_or("").to_string(),
                r["tool_schema_hash"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_read_only_reports_answer_in_place_rather_than_spending_a_turn() {
    let f = fixture();
    let provider = Fake::named("claude-opus-5", Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    for (line, expect) in [
        ("/help", "THE INTERACTIVE SESSION"),
        ("/config", "no model was called"),
        ("/agents", "sessions"),
        ("/resume", "emma --resume"),
    ] {
        f.command(&mut agent, &mut current, &running, line).await;
        assert!(
            f.said().contains(expect),
            "`{line}` did not answer: {}",
            f.said()
        );
    }
    // The whole point of answering here: not one model call was made.
    assert_eq!(provider.calls(), 0);
}

/// `/config` must report the model that is *running*, not the one on disk —
/// which is the one thing a user who has just typed `/model` is checking.
#[tokio::test]
async fn config_reports_the_running_model_rather_than_the_stored_one() {
    let f = fixture();
    let provider = Fake::named("claude-opus-5", Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    f.command(
        &mut agent,
        &mut current,
        &running,
        "/model claude-sonnet-4-5",
    )
    .await;
    assert_eq!(current.model_id(), "claude-sonnet-4-5");
    // The third holder of the same fact. A delegation resolves its provider
    // from this cell when it starts, so a `/model` that updated the agent and
    // `main` but not this one would put a subagent on the model the user had
    // just changed away from — silently, and with the status row disagreeing.
    assert_eq!(running.get().model_id(), "claude-sonnet-4-5");
    f.command(&mut agent, &mut current, &running, "/config")
        .await;
    let said = f.said();
    assert!(said.contains("claude-sonnet-4-5  (this session"), "{said}");
}

/// `/theme` end to end, through the seam `main` uses.
///
/// The two halves that matter are on opposite sides of the same command: a name
/// that is not there must leave `settings.json` exactly as it found it, and a
/// name that is there must land in it without disturbing anything else living
/// in that file.
#[tokio::test]
async fn theme_writes_the_selection_only_for_a_name_that_is_really_there() {
    let f = fixture();
    let provider = Fake::named("claude-opus-5", Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    let settings = f.dir.path().join(".emma").join("settings.json");
    std::fs::write(
        &settings,
        r#"{"provider":"anthropic","models":{"anthropic":"claude-x"}}"#,
    )
    .unwrap();

    // Nothing is there yet, so the empty list has to answer by itself.
    f.command(&mut agent, &mut current, &running, "/theme")
        .await;
    let said = f.said();
    assert!(said.contains("no theme files were found"), "{said}");

    // A name that does not exist changes nothing on disk.
    f.command(&mut agent, &mut current, &running, "/theme oxide")
        .await;
    let raw = std::fs::read_to_string(&settings).unwrap();
    assert!(!raw.contains("theme"), "{raw}");
    let said = f.said();
    assert!(said.contains("no theme called `oxide`"), "{said}");

    // A file that is there and will not parse is the same answer, and this is
    // the half worth having end to end: the loader would fall back with a
    // notice rather than break the boot, so nothing stops the name being
    // written except the check that happens before the write.
    let themes = f.dir.path().join(".emma").join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    std::fs::write(themes.join("broken.json"), "{ not json").unwrap();
    f.command(&mut agent, &mut current, &running, "/theme broken")
        .await;
    let raw = std::fs::read_to_string(&settings).unwrap();
    assert!(!raw.contains("theme"), "{raw}");
    let said = f.said();
    assert!(said.contains("is not JSON"), "{said}");

    std::fs::write(themes.join("oxide.json"), r#"{"about":"warmer"}"#).unwrap();
    assert_eq!(
        f.command(&mut agent, &mut current, &running, "/theme oxide")
            .await,
        Flow::Continue
    );
    let back: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(back["theme"], "oxide");
    // The key beside it, which a write that replaced the document would have
    // taken with it.
    assert_eq!(back["models"]["anthropic"], "claude-x");
    let said = f.said();
    assert!(said.contains("the next start"), "{said}");
}

// endregion: What a command must not do

// region: A value the tests above lean on

/// `Value` is used by `working_goal`'s `json!`; naming it keeps the import
/// honest rather than relying on a macro's expansion.
const _: Option<Value> = None;

// endregion

/// Automatic compaction that cannot help says so, once.
///
/// **The silent half of a pair.** `/compact` has always explained itself; the
/// threshold path discarded the same explanation. So a conversation over its
/// cap with nothing left to summarise grew on every call in silence until the
/// token budget or a provider 400 ended it, and the user's first sign was the
/// run stopping. Said once rather than per request, because the condition holds
/// from then on and a warning per call would bury the run in one sentence.
#[tokio::test]
async fn automatic_compaction_that_cannot_help_says_so_once() {
    let f = fixture();
    let provider = Fake::new(vec![
        text("one\n\nGOAL COMPLETE"),
        text("two\n\nGOAL COMPLETE"),
        text("three\n\nGOAL COMPLETE"),
    ]);
    // A cap of one token: every request is over it, and two four-word goals
    // cannot be summarised into anything smaller than the note that replaces
    // them.
    let budgets = Budgets {
        max_context: 1,
        ..Budgets::default()
    };
    let mut agent = f.agent_with(provider.clone(), budgets);
    agent.run_goal(&Goal::new("say one")).await;
    agent.run_goal(&Goal::new("say two")).await;
    agent.run_goal(&Goal::new("say three")).await;

    let said = f.said();
    assert!(
        said.contains("compaction cannot"),
        "the automatic path stayed silent about being stuck: {said}"
    );
    assert_eq!(
        said.matches("compaction cannot").count(),
        1,
        "said more than once: {said}"
    );
}

/// `config check` says which skills were skipped, and how many.
///
/// **A number nothing can read is a comment, and this row had been reduced to
/// exactly that three times.** `HARD-001` was filed because skipped skills were
/// named on stderr and never counted; its first fix added a count — on stderr.
/// Its second added `Harness::skill_notes()` and a test asserting the accessor,
/// and no production code called it.
///
/// **The third was this test, and it was mine.** It carried a doc comment
/// saying the assertion was on `config check`'s output and that deleting either
/// call site would turn it red. It did neither: it iterated `skill_notes()`
/// into a local buffer, which is the accessor again, one layer of prose away
/// from the same mistake. A reviewer read the doc against the body and said so.
///
/// So it drives `config_check` and reads what that function actually writes.
/// Delete the loop at `commands.rs`'s skill-notes call site and this goes red.
/// `config check` names a deny rule that can never fire.
///
/// **The announcement had exactly one delivery site and no test could reach
/// it.** `main` warns about inert rules at boot; that is a `term.warn` no test
/// can drive, so an independent reviewer replaced the whole call with a discard
/// and the entire workspace stayed green. The row's title is literally that
/// such rules "are NOT announced", and the only assertion was on the accessor.
///
/// It is also the wrong command to be silent. Somebody runs `config check`
/// *because* a deny rule did not bite — the boot warning has scrolled away, or
/// the rule was written after the session started. The reviewer's words: the
/// command an operator reaches for when a protection failed said nothing about
/// the protection being impossible.
///
/// So the delivery moved somewhere a test can read it, which is the same shape
/// as `ARCH-003`'s pilot: the decision was already covered, the effect was not.
#[test]
fn config_check_names_a_deny_rule_that_can_never_fire() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".claude");
    std::fs::create_dir_all(&root).unwrap();

    // `Bash(curl https://*)` strips to a prefix ending mid-word, and
    // `command_matches` is a word-boundary test — so it matches nothing however
    // it is spelled. The operator wrote a deny and has no protection.
    //
    // The second rule is the control: an ordinary deny that fires perfectly
    // well and must NOT be announced. A note on every rule is noise, and would
    // also make the assertion above pass for the wrong reason.
    std::fs::write(
        root.join("settings.json"),
        r#"{"permissions":{"deny":["Bash(curl https://*)","Bash(rm -rf /)"]}}"#,
    )
    .unwrap();

    let harness = emma_harness::Harness::load(&root).expect("load");
    // **`Bash` has to be a real tool in this run.** With an empty registry both
    // rules get the *other* inert note -- "names `Bash`, which is not a tool in
    // this run" -- which is correct for that fixture and not what is being
    // tested. A registry that does not hold the tool the rule names cannot
    // exercise the word-boundary branch at all.
    let (bash, _calls) = TestTool::returning("Bash", "ran");
    let tools = registry(vec![bash]);

    let mut out: Vec<u8> = Vec::new();
    emma::commands::config_check(&harness, &tools, dir.path(), &[], None, &mut out)
        .expect("config check");
    let text = String::from_utf8(out).expect("utf8");

    assert!(
        text.contains("! ") && text.contains("curl https://"),
        "config check did not name the rule that cannot fire: {text}"
    );
    assert!(
        text.contains("matches only the exact command"),
        "config check named the rule without saying what it does match: {text}"
    );
    // **The sentence that shipped, refused by name.** It said such a rule "can
    // never match", which is false of every rule this branch sees: equality is
    // tried before the word-boundary test, so a non-empty prefix matches at
    // least the command it spells. An operator who believed it would delete a
    // deny rule that works, and this is the channel they would read it on.
    assert!(
        !text.contains("never match"),
        "config check told the operator a rule can never fire. It can, and this \
         is the output somebody acts on: {text}"
    );
    // The exact-match rule stays silent. Asserted against the RULE rather than
    // against a phrase, so it survives the note being reworded again -- the
    // previous version of this assertion looked for wording that the fix
    // removed, which would have left it passing against anything.
    assert!(
        !text.contains("`Bash(rm -rf /)` matches only"),
        "an exact-match rule that fires was announced as dead, which is how a \
         live protection gets deleted: {text}"
    );
}

/// `config check` names frontmatter keys that were written and do nothing.
///
/// **The trap being closed: "I wrote `allowed-tools:` and it did nothing".**
/// Emma honours `tools:` on an agent and nothing else anywhere, and a command's
/// whole block is discarded, so a `description:` meant for a menu never reaches
/// one. That is defensible -- `.claude` files are written for Claude Code,
/// which has keys Emma has no business acting on -- but being unable to ask was
/// not.
///
/// **Asserted on the operator's channel, not on the accessor.** The accessor
/// version of this test is the mistake `HARD-001` made twice and `DEF-043` made
/// again this week: a test that loops the getter into a buffer stays green when
/// the call site that prints it is deleted.
#[tokio::test]
async fn config_check_names_frontmatter_keys_that_do_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".claude");

    // A skill carrying keys Claude Code understands and Emma does not.
    std::fs::create_dir_all(root.join("skills").join("thing")).unwrap();
    std::fs::write(
        root.join("skills").join("thing").join("SKILL.md"),
        // The nested block is here on purpose: its indented lines are VALUES,
        // and a scanner that read them as keys would report `path` and `write`
        // as inert and make the count meaningless.
        "---
name: thing
description: does a thing
allowed-tools: Bash
model: opus
hooks:
  path: x
  write: y
---
body
",
    )
    .unwrap();

    // A command whose whole block is discarded.
    std::fs::create_dir_all(root.join("commands")).unwrap();
    std::fs::write(
        root.join("commands").join("go.md"),
        "---
description: run the thing
argument-hint: <path>
---
do it
",
    )
    .unwrap();

    // And an agent that uses only keys Emma really honours, so the report is a
    // report and not a list of every file.
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::write(
        root.join("agents").join("helper.md"),
        "---
name: helper
description: helps
tools: Read
model: sonnet
---
be helpful
",
    )
    .unwrap();

    let harness = emma_harness::Harness::load(&root).expect("this harness must load");
    let tools = emma_tool_api::Registry::default();
    let mut out: Vec<u8> = Vec::new();
    emma::commands::config_check(&harness, &tools, dir.path(), &[], None, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();

    assert!(
        text.contains("allowed-tools"),
        "the key most likely to be written and least likely to work was not          named:
{text}"
    );
    assert!(
        text.contains("argument-hint"),
        "a command's discarded frontmatter was not reported:
{text}"
    );
    assert!(
        text.contains("2 file(s)"),
        "the count is missing or wrong -- the agent uses only honoured keys and          must not be listed:
{text}"
    );
    // Scoped to the line in question rather than the whole output. The loose
    // version of this -- `!text.contains("path")` -- failed against other lines
    // of `config check` that legitimately carry the word, which is the same
    // false-positive as `DEF-043`'s `contains('1')` with the sign flipped.
    let skill_line = text
        .lines()
        .find(|l| l.contains("SKILL.md"))
        .expect("the skill was not reported at all");
    assert!(
        !skill_line.contains("path") && !skill_line.contains("write"),
        "an indented VALUE was reported as a key, which makes the count a measure \
         of indentation rather than of what is inert: {skill_line:?}"
    );
    assert!(
        skill_line.contains("hooks"),
        "the KEY of a nested block was skipped along with its values: {skill_line:?}"
    );
    assert!(
        !text.contains("helper.md"),
        "a file whose every key is honoured was reported as inert, which would          make the report a list of every file and therefore useless:
{text}"
    );
}

#[test]
fn config_check_names_the_skills_it_skipped_and_counts_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".claude");
    let skills = root.join("skills");

    // One good skill, so the catalogue is not empty and the count below is a
    // shortfall rather than a total failure.
    std::fs::create_dir_all(skills.join("good")).unwrap();
    std::fs::write(
        skills.join("good").join("SKILL.md"),
        "---\nname: good\ndescription: a usable skill\n---\nbody\n",
    )
    .unwrap();

    // One that cannot load: frontmatter that never closes.
    std::fs::create_dir_all(skills.join("broken")).unwrap();
    std::fs::write(
        skills.join("broken").join("SKILL.md"),
        "---\nname: broken\ndescription: never closes\n",
    )
    .unwrap();

    let harness = emma_harness::Harness::load(&root).expect("one bad skill must not stop the boot");
    let tools = emma_tool_api::Registry::default();

    let mut out: Vec<u8> = Vec::new();
    emma::commands::config_check(&harness, &tools, dir.path(), &[], None, &mut out)
        .expect("config check must not fail on a harness with one bad skill");
    let text = String::from_utf8(out).unwrap();

    assert!(
        text.contains("skipped"),
        "`config check` told the operator nothing about the skill that did not \
         load:\n{text}"
    );
    // **The count is asserted where it is spoken, not as a loose digit.** This
    // read `text.contains('1')` until an independent reviewer pointed out that
    // `config check` prints hex hashes, so a `1` is in the output whether or not
    // the count is: they removed the count and the test stayed green. Anchoring
    // it to the phrase the note actually uses is what makes it able to fail --
    // the same repair, for the same reason, as `DEF-006` and `DEF-013`.
    assert!(
        text.contains("1 skill(s)"),
        "the shortfall was named without a count, which is what HARD-001 was \
         filed over:\n{text}"
    );
    assert!(
        text.contains("were skipped and are not in the catalogue"),
        "the count was printed without saying what it counts:\n{text}"
    );
    assert!(
        harness.skill_names().contains(&"good"),
        "the usable skill was lost along with the broken one"
    );
}

/// `config check` says which command files were passed over, and how many.
///
/// **The same defect as `HARD-001`, in the sibling directory, found by review.**
/// `load_commands` counted nested files into an `eprintln!` and returned nothing,
/// so the number reached stderr and no further — and the only test asserted
/// `command_names() == ["top"]`, which stays true whether the nested files are
/// counted, reported, or ignored entirely. `HARD-001`'s own text had already
/// named the fix: `load_skills` returned its notes and `Harness` exposed them,
/// one file away.
///
/// Asserted on `config check`'s output, not on the accessor, because asserting
/// on the accessor is the mistake this row's neighbour made twice.
#[test]
fn config_check_names_the_command_files_it_passed_over() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".claude");
    let commands = root.join("commands");
    std::fs::create_dir_all(commands.join("nested")).unwrap();
    std::fs::write(
        commands.join("top.md"),
        "a top-level command
",
    )
    .unwrap();
    std::fs::write(
        commands.join("nested").join("buried.md"),
        "not loaded
",
    )
    .unwrap();

    let harness =
        emma_harness::Harness::load(&root).expect("a nested command must not stop the boot");
    let tools = emma_tool_api::Registry::default();

    let mut out: Vec<u8> = Vec::new();
    emma::commands::config_check(&harness, &tools, dir.path(), &[], None, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();

    assert!(
        text.contains("subdirectories"),
        "`config check` said nothing about the command file it passed over:
{text}"
    );
    // **`text.contains('1')` was the assertion here, and it could not fail.**
    // `config check` prints paths, rule counts and a skills total, so some
    // digit is always somewhere in the output. A reviewer hardcoded the note to
    // say "0 command file(s)" -- a flat lie about a fixture with exactly one
    // nested command -- and this test stayed green.
    //
    // The count is now read out of the note itself, so a wrong number fails
    // rather than a wrong character class passing.
    assert!(
        text.contains("1 command file(s)"),
        "the shortfall was named without the count, or with the wrong one:
{text}"
    );
    assert!(
        harness.command_names().contains(&"top"),
        "the top-level command was lost along with the nested one"
    );
}

// region: /export
// ---------------------------------------------------------------------------
// /export
//
// The command with no test at all until now, and the one that writes to the
// user's filesystem. Everything here is asserted on the file that lands and on
// the sentence the user is shown, because those are the two things a person
// acts on: they go looking for a file at the path they were told.
// ---------------------------------------------------------------------------

/// If this fails, `/export` writes a file the user cannot find, or reports a
/// turn count that does not describe what is in it.
#[tokio::test]
async fn export_writes_the_conversation_beside_the_log_and_names_the_file() {
    let f = fixture();
    let mut script = working_goal();
    script.extend(working_goal());
    let provider = Fake::new(script);
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    agent.run_goal(&Goal::new("the first question")).await;
    agent.run_goal(&Goal::new("the second question")).await;

    let beside = f.log.path().with_extension("md");
    assert!(!beside.exists(), "the export existed before /export ran");
    f.command(&mut agent, &mut current, &running, "/export")
        .await;

    let written = std::fs::read_to_string(&beside).expect("/export wrote no file");
    // Both goals, as headings, and the answers under them. A file that held
    // only the last turn would still be a file at the promised path.
    assert!(written.contains("## the first question"), "{written}");
    assert!(written.contains("## the second question"), "{written}");
    assert!(written.contains("done"), "{written}");

    let said = f.said();
    // The path is the whole of what the user does next with this message.
    assert!(
        said.contains(&format!("/export: wrote 4 turn(s) to {}", beside.display())),
        "{said}"
    );
    // The damage banner is for a damaged log. A warning that fires on a clean
    // session is a warning nobody reads on a dirty one.
    assert!(
        !said.contains("could not be read"),
        "a clean session was reported as damaged: {said}"
    );
    assert!(!written.contains("This export is incomplete"), "{written}");
}

/// If this fails, `/export somewhere.md` drops a copy of the conversation in a
/// second place the user did not ask for — beside the session log, which is
/// under their home directory.
#[tokio::test]
async fn export_to_a_named_path_writes_there_and_nowhere_else() {
    let f = fixture();
    let provider = Fake::new(working_goal());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    agent.run_goal(&Goal::new("the only question")).await;

    let target = f.dir.path().join("chosen.md");
    let beside = f.log.path().with_extension("md");
    f.command(
        &mut agent,
        &mut current,
        &running,
        &format!("/export {}", target.display()),
    )
    .await;

    let written = std::fs::read_to_string(&target).expect("nothing at the named path");
    assert!(written.contains("## the only question"), "{written}");
    assert!(
        !beside.exists(),
        "a named path was honoured and the default one was written as well"
    );
    assert!(
        f.said().contains(&target.display().to_string()),
        "{}",
        f.said()
    );
}

/// If this fails, `/export` on a session that has said nothing leaves an empty
/// file on disk and reports having written it — and the user believes their
/// conversation is in it.
#[tokio::test]
async fn export_with_no_conversation_writes_no_file_and_says_why() {
    let f = fixture();
    let provider = Fake::new(Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    f.command(&mut agent, &mut current, &running, "/export")
        .await;

    let beside = f.log.path().with_extension("md");
    assert!(!beside.exists(), "an empty export was written to disk");
    let said = f.said();
    assert!(
        said.contains("/export: nothing to write — this session has no conversation yet."),
        "{said}"
    );
    // The success sentence must not also fire. "wrote 0 turn(s)" is the shape
    // of report that sends somebody looking for a file that is not there.
    assert!(!said.contains("wrote"), "{said}");
}

/// If this fails, a session whose log was damaged exports as though it were
/// whole — and the missing turns are invisible in the only copy the user keeps.
#[tokio::test]
async fn export_says_in_the_file_and_on_screen_that_a_damaged_log_is_incomplete() {
    let f = fixture();
    let mut script = working_goal();
    script.extend(working_goal());
    let provider = Fake::new(script);
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    agent.run_goal(&Goal::new("the first question")).await;

    // Damage in the middle of the file, not the torn last line a crash leaves:
    // the trailing record below is what makes the bad line a middle one.
    {
        use std::io::Write as _;
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(f.log.path())
            .unwrap();
        writeln!(log, "{{ this line is not json").unwrap();
        writeln!(log, r#"{{"kind":"note","text":"after the damage"}}"#).unwrap();
    }

    f.command(&mut agent, &mut current, &running, "/export")
        .await;

    let beside = f.log.path().with_extension("md");
    let written = std::fs::read_to_string(&beside).expect("/export wrote no file");
    assert!(
        written.contains("record(s) of this session could not be read"),
        "the file reads as a complete transcript: {written}"
    );
    assert!(written.contains("This export is incomplete"), "{written}");
    let said = f.said();
    assert!(
        said.contains("could not be read and are missing from the file"),
        "the loss reached the file and not the person: {said}"
    );
}

// endregion: /export

// region: /copy
// ---------------------------------------------------------------------------
// /copy
// ---------------------------------------------------------------------------

/// If this fails, `/copy` on a run that emits no escape bytes reports having
/// copied something. Nothing reached the clipboard, the user pastes whatever
/// was there before, and `INV-001` is what stopped the bytes — so the report is
/// a claim about a write that was refused two layers down.
///
/// The unframed arm is the only one a test can reach: `Term::recording` has no
/// viewport, and `Term::clipboard` refuses without one. The framed arm is
/// **unverified here** and is named in the report.
#[tokio::test]
async fn copy_on_a_run_with_no_escape_bytes_refuses_and_points_at_export() {
    let f = fixture();
    let provider = Fake::new(working_goal());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    agent.run_goal(&Goal::new("the only question")).await;
    assert!(!f.term.framed(), "the fixture grew a viewport");

    f.command(&mut agent, &mut current, &running, "/copy").await;
    let said = f.said();
    // The claim that must never appear on this path, keyed on the exact prefix
    // of the success line. Two looser spellings do not work and both were tried:
    // `"sent"` matches the refusal's own "nothing was sent", and
    // `"to the clipboard"` matches the refusal's first clause — which is how
    // this assertion failed the first time it was run, against correct code.
    assert!(
        !said.contains("/copy: sent"),
        "/copy claimed a clipboard write on a run that emits no escape bytes: {said}"
    );
    // …and the escape hatch is named, because "it did not work" with no next
    // step is the report that comes back as a bug.
    assert!(said.contains("/export"), "{said}");
    assert!(
        said.contains("no escape bytes"),
        "the refusal did not say why: {said}"
    );
}

// endregion: /copy

// region: A built-in name with arguments it does not take
// ---------------------------------------------------------------------------
// Misuse
//
// Three commands reach `SessionCommand::Misuse`, and until now nothing drove
// any of them through `run`. The parser's own tests stop at the variant; what
// they cannot see is whether the usage line is ever printed, and whether the
// command does anything on its way to printing it.
// ---------------------------------------------------------------------------

/// If this fails, a mistyped command either does its job with arguments the
/// user did not mean — `/export --force` writing a file somewhere nobody named
/// — or answers with nothing at all, which reads as Emma having hung.
#[tokio::test]
async fn a_misused_command_answers_with_its_usage_and_changes_nothing() {
    let f = fixture();
    let provider = Fake::named("model-one", working_goal());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    agent.run_goal(&Goal::new("the only question")).await;

    let settings = f.dir.path().join(".emma").join("settings.json");
    let beside = f.log.path().with_extension("md");

    f.command(&mut agent, &mut current, &running, "/export --force")
        .await;
    f.command(&mut agent, &mut current, &running, "/theme --save")
        .await;
    f.command(&mut agent, &mut current, &running, "/model the api surface")
        .await;

    let said = f.said();
    // The header, per command, and one line of the usage each — the sentence
    // that tells the reader what to type instead.
    assert!(said.contains("/export takes:"), "{said}");
    assert!(
        said.contains("/export <path>     write it to that file instead"),
        "{said}"
    );
    assert!(said.contains("/theme takes:"), "{said}");
    assert!(said.contains("There is no --save"), "{said}");
    assert!(said.contains("/model takes:"), "{said}");
    assert!(
        said.contains("/model <id>                use <id> for the rest of this session"),
        "{said}"
    );

    // And not one of the three did its job on the way past.
    assert!(!beside.exists(), "/export --force wrote a file anyway");
    assert!(!settings.exists(), "a misuse wrote settings.json");
    assert_eq!(
        current.model_id(),
        "model-one",
        "/model with a sentence changed the model"
    );
    assert_eq!(running.get().model_id(), "model-one");
}

// endregion: Misuse

// region: /model --save
// ---------------------------------------------------------------------------
// The half of /model that outlives the session
// ---------------------------------------------------------------------------

/// If this fails, either `--save` does not remember the model — the next
/// session silently starts on the old one — or a plain `/model <id>` writes to
/// `settings.json` without being asked, which changes every future session from
/// a command whose own output says it did not.
#[tokio::test]
async fn model_writes_settings_only_when_save_is_asked_for_and_says_which_it_did() {
    let f = fixture();
    let provider = Fake::named("model-one", Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    let settings = f.dir.path().join(".emma").join("settings.json");

    f.command(
        &mut agent,
        &mut current,
        &running,
        "/model claude-sonnet-4-5",
    )
    .await;
    assert!(
        !settings.exists(),
        "a /model with no --save wrote the choice to disk"
    );
    let said = f.said();
    assert!(
        said.contains("settings  unchanged. `/model claude-sonnet-4-5 --save` remembers it"),
        "{said}"
    );

    f.command(
        &mut agent,
        &mut current,
        &running,
        "/model claude-opus-5 --save",
    )
    .await;
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap())
        .expect("--save wrote no readable settings.json");
    assert_eq!(
        stored["models"]["anthropic"], "claude-opus-5",
        "--save did not record the model for this provider: {stored}"
    );
    let said = f.said();
    assert!(
        said.contains(&format!(
            "settings  claude-opus-5 written to {}",
            settings.display()
        )),
        "{said}"
    );
}

/// If this fails, `/model <the model already running>` reports a model change
/// that did not happen — including the paragraph saying the cached prefix is
/// gone, which is a bill the user has not been charged.
#[tokio::test]
async fn model_set_to_what_is_already_running_says_so_and_claims_no_cache_loss() {
    let f = fixture();
    let provider = Fake::named("claude-opus-5", Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    f.command(&mut agent, &mut current, &running, "/model claude-opus-5")
        .await;
    let said = f.said();
    assert!(
        said.contains("claude-opus-5 is already what this session is using."),
        "{said}"
    );
    // The two lines a real change prints. Either of them here is a report of
    // work that was not done.
    assert!(
        !said.contains("the cached prefix is gone"),
        "a no-op /model told the user their cache had been thrown away: {said}"
    );
    assert!(
        !said.contains("for the rest of this session (was"),
        "a no-op /model reported a change: {said}"
    );
}

// endregion: /model --save

// region: What /clear kept
// ---------------------------------------------------------------------------
// The receipt, in both of its forms
//
// `clear_says_what_it_kept_…` above asserts `contains("grant")`, and the empty
// arm — "no tool or host grants have been given this session" — contains that
// word too. So the two arms have never been told apart. These two do it.
// ---------------------------------------------------------------------------

/// If this fails, `/clear` names grants that were never given. The receipt's
/// whole job is that a user can see what consent survived; one that lists
/// something in a session where nothing was granted teaches them to ignore it.
#[tokio::test]
async fn clear_with_nothing_granted_says_nothing_was_granted() {
    let f = fixture();
    let provider = Fake::new(working_goal());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    agent.run_goal(&Goal::new("first")).await;

    f.command(&mut agent, &mut current, &running, "/clear")
        .await;
    let said = f.said();
    assert!(
        said.contains("kept      no tool or host grants have been given this session"),
        "{said}"
    );
    assert!(
        !said.contains("you will not be asked about them again"),
        "a session with no grants was told its grants were kept: {said}"
    );
}

/// If this fails, a tool the user allowed for the process survives `/clear` in
/// silence. `/clear` is the moment somebody expects a reset, and consent that
/// quietly outlives it is what the receipt exists to make visible.
#[tokio::test]
async fn clear_names_the_tool_grant_it_is_keeping() {
    let f = fixture_asking(vec![Answer::AlwaysThisTool]);
    let provider = Fake::new(vec![
        call("Edit", serde_json::json!({ "x": "1" })),
        text("done\n\nGOAL COMPLETE"),
    ]);
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());
    agent.run_goal(&Goal::new("first")).await;

    f.command(&mut agent, &mut current, &running, "/clear")
        .await;
    let said = f.said();
    // The tool by name, and the sentence that says how long it lasts. A count
    // with no name is a receipt nobody can act on.
    assert!(said.contains("kept      1 tool grant (Edit)"), "{said}");
    assert!(
        said.contains("you will not be asked about them again until you /exit"),
        "{said}"
    );
    assert!(
        !said.contains("no tool or host grants have been given"),
        "a granted tool was reported as no grant at all: {said}"
    );
}

// endregion: What /clear kept

// region: /agents
// ---------------------------------------------------------------------------
// /agents
//
// The existing assertion is `contains("sessions")`, which is the first word of
// the first line and is printed whatever the data says. These two read the part
// that depends on what was actually recorded.
// ---------------------------------------------------------------------------

/// If this fails, `/agents` on a machine that has delegated nothing prints an
/// empty table instead of saying there is nothing to compare — and the user
/// reads a blank report as a broken command.
#[tokio::test]
async fn agents_with_no_delegations_says_there_is_nothing_to_compare() {
    let f = fixture();
    let provider = Fake::new(Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    f.command(&mut agent, &mut current, &running, "/agents")
        .await;
    let said = f.said();
    assert!(
        said.contains("Nothing has been delegated from this machine yet"),
        "{said}"
    );
    // The header of the table that must not be there. Asserted on its columns
    // rather than on emptiness, because a header with no rows under it is
    // exactly the output this is written against.
    assert!(!said.contains("tokens/run"), "{said}");
}

/// If this fails, `/agents` does not count what the harness recorded — the
/// command's entire purpose — and a user comparing subagent types is reading
/// numbers that came from somewhere other than the logs.
#[tokio::test]
async fn agents_counts_the_delegations_that_were_recorded() {
    let f = fixture();
    let provider = Fake::new(Vec::new());
    let mut agent = f.agent(provider.clone());
    let mut current: Arc<dyn Provider> = provider.clone();
    let running = Running::new(provider.clone());

    // A second session file in the same directory, holding one delegation. It
    // is written rather than driven because `/agents` reads the recorded
    // account and nothing else — which is the property being pinned.
    std::fs::write(
        f.dir.path().join("sess-earlier.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "kind": "delegation",
                "agent": "scout",
                "ending": "done",
                "cost_tokens": 4321,
                "elapsed_ms": 7000,
                "iterations": 3,
                "tool_calls": 5,
                "task": "look at the manifest"
            })
        ),
    )
    .unwrap();

    f.command(&mut agent, &mut current, &running, "/agents")
        .await;
    let said = f.said();
    assert!(said.contains("delegations    1"), "{said}");
    // The row, with the name and the finished-of-runs ratio that is the whole
    // reason to run this command.
    assert!(said.contains("scout"), "{said}");
    assert!(said.contains("1/1"), "{said}");
    assert!(said.contains("4321"), "{said}");
    assert!(said.contains("look at the manifest"), "{said}");
    assert!(
        !said.contains("Nothing has been delegated"),
        "a recorded delegation was reported as none: {said}"
    );
}

// endregion: /agents
