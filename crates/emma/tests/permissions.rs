//! The persisted grant, end to end, through the real loop and a real file.
//!
//! Everything in `approval.rs` and `permissions.rs` is tested against `decide`
//! and against a `Rule`. What is left over is the claim the owner actually made
//! — *"as you approve it keeps the auto approvals in json in the repo"* — and
//! that claim spans two processes and a file between them. So these tests run
//! the real `Agent` against a real harness directory twice: once with a human
//! answering, once with nobody answering at all, and the only thing carried
//! between them is the JSON on disk.
//!
//! The second run's approvals gate is built from what
//! `Harness::permissions()` read back off the disk, exactly the way `main`
//! builds it. That is the point — nothing is handed from the first run to the
//! second in memory, so an `Allow` in run two can only have come out of the
//! file.

mod support;

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use emma::agent::{Agent, Budgets, Ending, Interrupt, Outcome, Setup};
use emma::approval::{Answer, Approvals, Asker, Gate};
use emma::goal::{Goal, MarkerClaim};
use emma::permissions::{file_for, Rules};
use emma::session::SessionLog;
use emma::term::Term;
use emma_harness::{Flavor, Harness};
use emma_llm::{Caching, Mode};
use emma_tool_api::Registry;
use serde_json::json;

use support::{call, registry, text, Fake, TestTool};

fn budgets() -> Budgets {
    Budgets {
        max_iterations: 20,
        max_tokens: 1_000_000,
        wall_clock: Duration::from_secs(60),
        max_kicks: 3,
        max_context: 1_000_000,
    }
}

/// A `.claude/` harness, because that is the directory this feature's format
/// belongs to and the one a real project most often has.
fn harness_dir(dir: &Path, settings: &str) -> std::path::PathBuf {
    let root = dir.join(".claude");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("settings.json"), settings).unwrap();
    root
}

/// One goal, one scripted model, one gate. Built the way `main` builds it: the
/// rules come from the harness and the write target from `file_for`.
async fn run(
    root: &Path,
    cwd: &Path,
    tools: Registry,
    approvals: &Approvals,
    fake: &Fake,
) -> Outcome {
    let harness = Harness::load_selecting(root, Flavor::Claude, None).unwrap();
    let term = Term::silent();
    let log = SessionLog::none();
    let mut agent = Agent::new(Setup {
        provider: fake,
        harness: &harness,
        instructions: &harness.instructions,
        tools: &tools,
        approvals,
        log: &log,
        term: &term,
        interrupt: Interrupt::new(),
        spend: emma::agent::Spend::new(),
        done: &MarkerClaim,
        cwd: cwd.to_path_buf(),
        session_id: "sess-test".into(),
        budgets: budgets(),
        caching: Caching::On,
        mode: Mode::Batch,
    });
    agent.run_goal(&Goal::new("find out about the news")).await
}

/// The gate a fresh process would build in `root`: rules read off disk, and the
/// same file as the write target. `answers` is what the human types, which in
/// the second run of every test below is deliberately nothing.
fn gate(root: &Path, answers: Vec<Answer>) -> Approvals {
    let harness = Harness::load_selecting(root, Flavor::Claude, None).unwrap();
    let (rules, notes) = Rules::parse(harness.permissions());
    assert!(
        notes.is_empty(),
        "the file this run read is unusable: {notes:?}"
    );
    Approvals::new(Gate::Ask, Asker::Scripted(answers.into()))
        .with_rules(rules, Some(file_for(root)))
}

#[tokio::test]
async fn a_remembered_host_is_not_asked_about_in_the_next_process() {
    // The owner's sentence, as a test. Run one: a human answers `r`. Run two:
    // the answer queue is **empty**, so if the file did not carry the grant the
    // call is denied and the assertion fails.
    let dir = tempfile::tempdir().unwrap();
    let root = harness_dir(dir.path(), "{}");

    let (news, calls) = TestTool::reaching("WebFetch", "apnews.com");
    let out = run(
        &root,
        dir.path(),
        registry(vec![news.clone()]),
        &gate(&root, vec![Answer::RememberNarrow]),
        &Fake::new(vec![
            call("WebFetch", json!({ "x": "1" })),
            text("got it.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;
    assert_eq!(out.ending, Ending::Done);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // The file, as the user would find it. Asserted as parsed JSON rather than
    // a substring, because the shape is the interface: Claude Code has to be
    // able to read this back.
    let file = file_for(&root);
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        doc["permissions"]["allow"][0],
        "WebFetch(domain:apnews.com)"
    );
    assert_eq!(doc["permissions"]["allow"].as_array().unwrap().len(), 1);

    // Run two. Nobody is there to answer.
    let out = run(
        &root,
        dir.path(),
        registry(vec![news.clone()]),
        &gate(&root, Vec::new()),
        &Fake::new(vec![
            call("WebFetch", json!({ "x": "2" })),
            text("got it again.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;
    assert_eq!(out.ending, Ending::Done);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the second process asked again for a host that was written down"
    );

    // …and it did not widen. A second tool reaching the same host still asks,
    // because the rule names a tool; and the same tool reaching a different host
    // still asks, because the rule names a host.
    let (other_tool, other_tool_calls) = TestTool::reaching("WebSearch", "apnews.com");
    let (other_host, other_host_calls) = TestTool::reaching("WebFetch", "evil.example");
    let out = run(
        &root,
        dir.path(),
        registry(vec![other_tool, other_host]),
        &gate(&root, Vec::new()),
        &Fake::new(vec![
            call("WebSearch", json!({ "x": "3" })),
            call("WebFetch", json!({ "x": "4" })),
            text("both refused.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;
    assert_eq!(out.ending, Ending::Done);
    assert_eq!(other_tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(other_host_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn trusting_the_tool_covers_every_host_it_reaches_afterwards() {
    // "not just search, but like claude code" — the broad grant, and the reason
    // it is a separate keystroke. One `t` on `api.search.brave.com`, and the
    // next process reaches four hosts it has never been asked about.
    let dir = tempfile::tempdir().unwrap();
    let root = harness_dir(dir.path(), "{}");
    let (search, _) = TestTool::reaching("WebSearch", "api.search.brave.com");

    run(
        &root,
        dir.path(),
        registry(vec![search]),
        &gate(&root, vec![Answer::RememberWide]),
        &Fake::new(vec![
            call("WebSearch", json!({ "x": "1" })),
            text("ok.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file_for(&root)).unwrap()).unwrap();
    assert_eq!(doc["permissions"]["allow"][0], "WebSearch");

    // Four hosts, four tools sharing one name, one empty answer queue.
    for host in [
        "www.reuters.com",
        "tech.yahoo.com",
        "openai.com",
        "apnews.com",
    ] {
        let (tool, calls) = TestTool::reaching("WebSearch", host);
        let out = run(
            &root,
            dir.path(),
            registry(vec![tool]),
            &gate(&root, Vec::new()),
            &Fake::new(vec![
                call("WebSearch", json!({ "x": "1" })),
                text("ok.\n\nGOAL COMPLETE"),
            ]),
        )
        .await;
        assert_eq!(out.ending, Ending::Done);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "{host} was asked about despite a tool-wide grant"
        );
    }
}

#[tokio::test]
async fn a_deny_rule_in_the_project_file_beats_an_allow_in_the_local_one() {
    // The precedence guarantee where it costs something: the committed
    // `settings.json` refuses a host, the personal `settings.local.json` allows
    // it, and the refusal wins. If it inverted, any developer could grant
    // themselves past their team's `deny` list by answering one prompt.
    let dir = tempfile::tempdir().unwrap();
    let root = harness_dir(
        dir.path(),
        r#"{"permissions":{"deny":["WebFetch(domain:evil.example)"]}}"#,
    );
    std::fs::write(
        file_for(&root),
        r#"{"permissions":{"allow":["WebFetch(domain:evil.example)","WebFetch"]}}"#,
    )
    .unwrap();

    let (tool, calls) = TestTool::reaching("WebFetch", "evil.example");
    // Two `Yes` answers queued as well, so this cannot pass by the call simply
    // running out of answers: a human saying yes must not get past a deny rule
    // either.
    let out = run(
        &root,
        dir.path(),
        registry(vec![tool]),
        &gate(&root, vec![Answer::Yes, Answer::Yes]),
        &Fake::new(vec![
            call("WebFetch", json!({ "x": "1" })),
            text("blocked.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;
    assert_eq!(out.ending, Ending::Done);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a deny rule was overridden"
    );

    // The rest of the broad allow still works, so this is a deny that fires
    // rather than a gate that broke.
    let (elsewhere, elsewhere_calls) = TestTool::reaching("WebFetch", "docs.rs");
    run(
        &root,
        dir.path(),
        registry(vec![elsewhere]),
        &gate(&root, Vec::new()),
        &Fake::new(vec![
            call("WebFetch", json!({ "x": "1" })),
            text("fine.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;
    assert_eq!(elsewhere_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_hook_denial_outranks_an_allow_rule_that_covers_the_call() {
    // The oldest guarantee in the gate, checked against the newest way to get
    // past it. The `allow` rule covers this call completely — with no hook, the
    // call runs unprompted, which the previous test proves — and the hook denies
    // it anyway, because a `PreToolUse` denial is resolved before the gate is
    // consulted at all.
    let dir = tempfile::tempdir().unwrap();
    let root = support::harness_denying(dir.path(), "Runner");
    // The denying harness is a `.emma/`, so the rules go in its own spine.
    std::fs::write(file_for(&root), r#"{"permissions":{"allow":["Runner"]}}"#).unwrap();
    let harness = Harness::load_selecting(&root, Flavor::Emma, None).unwrap();
    let (rules, notes) = Rules::parse(harness.permissions());
    assert!(notes.is_empty(), "{notes:?}");

    let (runner, calls) = TestTool::ok("Runner", false);
    let term = Term::silent();
    let log = SessionLog::none();
    let approvals = Approvals::new(Gate::Ask, Asker::Scripted(Default::default()))
        .with_rules(rules, Some(file_for(&root)));
    let fake = Fake::new(vec![
        call("Runner", json!({ "x": "1" })),
        text("blocked by policy.\n\nGOAL COMPLETE"),
    ]);
    let tools = registry(vec![runner]);
    let mut agent = Agent::new(Setup {
        provider: &fake,
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
        mode: Mode::Batch,
    });
    let out = agent.run_goal(&Goal::new("run it")).await;
    assert_eq!(out.ending, Ending::Done);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "an allow rule in a settings file waved a PreToolUse denial through"
    );
    // …and the model was told it was policy, not a person, so it does not go
    // looking for a prompt that never comes.
    assert!(
        fake.transcript().contains("cannot be approved away"),
        "{}",
        fake.transcript()
    );
}

#[tokio::test]
async fn a_grant_written_into_a_file_that_already_has_other_keys_keeps_them() {
    // The write path that this project has been bitten by once already. The
    // local file here carries a `hooks` block and a `deny` list; after one
    // remembered grant, both must still be there — and the `deny` must still
    // fire, which is the half a round-trip test alone would not catch.
    let dir = tempfile::tempdir().unwrap();
    let root = harness_dir(dir.path(), "{}");
    let before = r#"{
  "statusLine": { "type": "command", "command": "hooks/status" },
  "permissions": { "deny": ["WebFetch(domain:evil.example)"] }
}"#;
    std::fs::write(file_for(&root), before).unwrap();

    let (news, _) = TestTool::reaching("WebFetch", "apnews.com");
    run(
        &root,
        dir.path(),
        registry(vec![news]),
        &gate(&root, vec![Answer::RememberNarrow]),
        &Fake::new(vec![
            call("WebFetch", json!({ "x": "1" })),
            text("ok.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file_for(&root)).unwrap()).unwrap();
    assert_eq!(doc["statusLine"]["command"], "hooks/status");
    assert_eq!(
        doc["permissions"]["deny"][0],
        "WebFetch(domain:evil.example)"
    );
    assert_eq!(
        doc["permissions"]["allow"][0],
        "WebFetch(domain:apnews.com)"
    );

    let (evil, evil_calls) = TestTool::reaching("WebFetch", "evil.example");
    run(
        &root,
        dir.path(),
        registry(vec![evil]),
        &gate(&root, Vec::new()),
        &Fake::new(vec![
            call("WebFetch", json!({ "x": "1" })),
            text("blocked.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;
    assert_eq!(
        evil_calls.load(Ordering::SeqCst),
        0,
        "the deny rule was lost when the allow rule was written beside it"
    );
}
