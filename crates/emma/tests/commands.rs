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
use emma::approval::Approvals;
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

impl Fixture {
    fn agent<'a>(&'a self, provider: Arc<dyn Provider>) -> Agent<'a> {
        Agent::new(Setup {
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
            budgets: Budgets::default(),
            caching: Caching::On,
            mode: Mode::Batch,
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
