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
//!
//! **One host proves the plumbing; six prove the matching.** The first version
//! of this file certified the mechanism against `example.com` alone, which
//! shows that a grant survives a process and shows nothing at all about whether
//! the rule that survived means what the user read. Host matching fails
//! quietly, and it fails in the direction that costs something: a rule that
//! grants *more* than was asked for passes every "the second run did not ask"
//! assertion in this file. So there are now three layers here, in order of how
//! much they can catch:
//!
//! 1. [`the_matching_matrix`] — a table of (rules, candidate host, verdict),
//!    six domains and every near-miss that makes matching hard. One line per
//!    case, each named, so a failure says which property broke.
//! 2. [`six_hosts_remembered_one_at_a_time_all_survive_in_one_file`] — the
//!    multi-run half: six separate grants written to one `settings.local.json`
//!    across six processes, read back by a seventh, with an **eighth host that
//!    must still ask**. That last assertion is the one that catches an
//!    over-broad rule; without it, a matcher that allowed everything would look
//!    like a pass.
//! 3. `live_grant` / `live_replay` — ignored by default, and the only tests
//!    here that touch a real network. See their comments for how they are run.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use emma::agent::{Agent, Budgets, Ending, Interrupt, Outcome, Setup};
use emma::approval::{Answer, Approvals, Asker, Gate};
use emma::goal::{Goal, MarkerClaim};
use emma::permissions::{file_for, Decision, Rules};
use emma::session::SessionLog;
use emma::term::Term;
use emma_harness::{Flavor, Harness, PermissionEntry, PermissionKind};
use emma_llm::{Caching, Mode};
use emma_tool_api::{NetworkTarget, Registry, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};

use support::{call, registry, text, Fake, TestTool};

/// The six hosts every multi-host test in this file uses, and the six the live
/// certification actually fetches.
///
/// Documentation domains and long-lived project sites, on purpose: they are
/// stable, they are safe to fetch once, and three of them (`example.com`,
/// `.org`, `.net`) share a label with each other, which is exactly the shape
/// that a sloppy matcher confuses.
const SIX: [&str; 6] = [
    "example.com",
    "example.org",
    "example.net",
    "www.iana.org",
    "www.rust-lang.org",
    "docs.rs",
];

/// A seventh host, never granted anywhere in this file. Every multi-host test
/// ends by asserting Emma still asks about it — the assertion that fails when a
/// rule grants more than it names, and passes for every other reason.
const UNGRANTED: &str = "www.rfc-editor.org";

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

// region: The matching matrix
// ---------------------------------------------------------------------------
// The matching matrix
//
// A table rather than a page of assertions, for one reason: the interesting
// cases here are near-misses, there are a lot of them, and each new one has to
// cost one line or it does not get written. Every row is named, and a failure
// reports every broken row rather than only the first — a matcher that has
// widened usually widens in several directions at once, and seeing all of them
// is what tells you which invariant went.
//
// Every row is asked through `Rules::for_egress`, the same call
// `approval::egress` makes, so what is under test is the path production uses
// and not a private helper.
// ---------------------------------------------------------------------------

/// One case: the rules in a settings file, a candidate host, and the verdict.
///
/// `rules` is an ordered list rather than three lists, because "which order
/// were they written in" is a question the file can answer and a bug can depend
/// on. Precedence must not care, and the rows below check both directions.
struct Row {
    name: &'static str,
    rules: Vec<(PermissionKind, &'static str)>,
    host: &'static str,
    want: Option<Decision>,
}

const fn allow(rule: &'static str) -> (PermissionKind, &'static str) {
    (PermissionKind::Allow, rule)
}
const fn deny(rule: &'static str) -> (PermissionKind, &'static str) {
    (PermissionKind::Deny, rule)
}
const fn ask(rule: &'static str) -> (PermissionKind, &'static str) {
    (PermissionKind::Ask, rule)
}

fn row(
    name: &'static str,
    rules: &[(PermissionKind, &'static str)],
    host: &'static str,
    want: Option<Decision>,
) -> Row {
    Row {
        name,
        rules: rules.to_vec(),
        host,
        want,
    }
}

impl Row {
    /// Parse this row's rules the way `main` parses a settings file, then ask
    /// the egress question.
    ///
    /// A rule that does not survive parsing is an `Err` rather than a panic, so
    /// one row that stops parsing does not hide the ninety-four behind it. It
    /// is still a failure — a rule Emma cannot evaluate matches nothing, which
    /// in a `deny` list is a protection that has quietly gone.
    fn verdict(&self) -> Result<Option<Decision>, String> {
        let entries: Vec<PermissionEntry> = self
            .rules
            .iter()
            .map(|(kind, rule)| PermissionEntry {
                rule: (*rule).to_string(),
                kind: *kind,
                source: PathBuf::from("settings.local.json"),
            })
            .collect();
        let (rules, notes) = Rules::parse(&entries);
        if !notes.is_empty() {
            return Err(format!("its rules no longer parse — {notes:?}"));
        }
        Ok(rules.for_egress("WebFetch", self.host))
    }
}

#[test]
fn the_matching_matrix() {
    use Decision::{Allow, Ask, Deny};

    // A rule that appears in most rows. Named so a row reads as the property
    // it is about rather than as a string.
    const COM: (PermissionKind, &str) = allow("WebFetch(domain:example.com)");

    #[rustfmt::skip]
    let matrix = vec![
        // -------------------------------------------------------------------
        // Exact match, over six distinct domains. The baseline: without these
        // every "does not match" below could pass by matching nothing at all.
        // -------------------------------------------------------------------
        row("exact/com", &[COM], "example.com", Some(Allow)),
        row("exact/org", &[allow("WebFetch(domain:example.org)")], "example.org", Some(Allow)),
        row("exact/net", &[allow("WebFetch(domain:example.net)")], "example.net", Some(Allow)),
        row("exact/iana", &[allow("WebFetch(domain:www.iana.org)")], "www.iana.org", Some(Allow)),
        row("exact/rust-lang", &[allow("WebFetch(domain:www.rust-lang.org)")], "www.rust-lang.org", Some(Allow)),
        row("exact/docs.rs", &[allow("WebFetch(domain:docs.rs)")], "docs.rs", Some(Allow)),
        // Sibling domains are not each other. Three rows, because the failure
        // this catches — matching on the leftmost label — is invisible when
        // only one of the three is in the table.
        row("exact/com-is-not-org", &[COM], "example.org", None),
        row("exact/com-is-not-net", &[COM], "example.net", None),
        row("exact/org-is-not-com", &[allow("WebFetch(domain:example.org)")], "example.com", None),
        // A grant for one project site is not a grant for the other.
        row("exact/iana-is-not-rust-lang", &[allow("WebFetch(domain:www.iana.org)")], "www.rust-lang.org", None),

        // -------------------------------------------------------------------
        // Scheme and port. `WebFetch` hands the gate `Url::host_str`, which
        // carries neither — so nothing production sends can look like these.
        // They are here because the matcher is a public function and the next
        // caller may not be so careful: a scheme or a port glued to the host
        // must fail closed, not match by prefix.
        // -------------------------------------------------------------------
        row("port/explicit-443-is-not-the-bare-host", &[COM], "example.com:443", None),
        row("port/explicit-8443", &[COM], "example.com:8443", None),
        row("scheme/https-prefix-is-not-a-host", &[COM], "https://example.com", None),
        row("scheme/url-with-a-path", &[COM], "example.com/admin", None),

        // -------------------------------------------------------------------
        // A subdomain against a bare-domain rule. The documented rule, and the
        // one people assume goes the other way: approving `example.com` has
        // never approved anything under it.
        // -------------------------------------------------------------------
        row("subdomain/one-level", &[COM], "sub.example.com", None),
        row("subdomain/www", &[COM], "www.example.com", None),
        row("subdomain/two-levels", &[COM], "a.b.example.com", None),
        row("subdomain/of-a-granted-www-host", &[allow("WebFetch(domain:www.iana.org)")], "data.www.iana.org", None),
        // …and the parent of a granted subdomain is not granted either. The
        // mirror image, and the one a suffix matcher gets right by accident.
        row("subdomain/parent-of-a-granted-host", &[allow("WebFetch(domain:www.iana.org)")], "iana.org", None),

        // -------------------------------------------------------------------
        // `*.example.com` — a strict subdomain at any depth.
        // -------------------------------------------------------------------
        row("star-left/one-level", &[allow("WebFetch(domain:*.example.com)")], "api.example.com", Some(Allow)),
        row("star-left/two-levels", &[allow("WebFetch(domain:*.example.com)")], "a.b.example.com", Some(Allow)),
        row("star-left/four-levels", &[allow("WebFetch(domain:*.example.com)")], "a.b.c.d.example.com", Some(Allow)),
        // The half everybody gets wrong: it does **not** cover the bare host.
        row("star-left/not-the-bare-host", &[allow("WebFetch(domain:*.example.com)")], "example.com", None),
        row("star-left/not-a-sibling-tld", &[allow("WebFetch(domain:*.example.com)")], "api.example.org", None),
        // A registrable lookalike wearing the pattern as a prefix.
        row("star-left/not-a-suffix-of-someone-elses-domain", &[allow("WebFetch(domain:*.example.com)")], "api.example.com.attacker.net", None),
        row("star-left/not-a-hyphenated-lookalike", &[allow("WebFetch(domain:*.example.com)")], "attacker-example.com", None),
        row("star-left/iana", &[allow("WebFetch(domain:*.iana.org)")], "www.iana.org", Some(Allow)),
        // `*.com` is legal, enormous, and exactly what it says. Pinned because
        // a reader should be able to find out from a test that this is a rule
        // covering every host under a TLD — and that it still stops at the TLD
        // itself.
        row("star-left/a-whole-tld-is-legal-and-huge", &[allow("WebFetch(domain:*.com)")], "anything.com", Some(Allow)),
        row("star-left/a-whole-tld-does-not-include-the-tld", &[allow("WebFetch(domain:*.com)")], "com", None),

        // -------------------------------------------------------------------
        // `example.*` — one label, and it cannot cross a dot.
        // -------------------------------------------------------------------
        row("star-right/org", &[allow("WebFetch(domain:example.*)")], "example.org", Some(Allow)),
        row("star-right/net", &[allow("WebFetch(domain:example.*)")], "example.net", Some(Allow)),
        row("star-right/com", &[allow("WebFetch(domain:example.*)")], "example.com", Some(Allow)),
        // The whole point of the label split: a star that crossed a dot would
        // hand `example.<anything an attacker registers>` to the user.
        row("star-right/does-not-cross-a-dot", &[allow("WebFetch(domain:example.*)")], "example.evil.com", None),
        row("star-right/does-not-cross-a-dot-into-a-cctld", &[allow("WebFetch(domain:example.*)")], "example.co.uk", None),
        row("star-right/needs-the-second-label", &[allow("WebFetch(domain:example.*)")], "example", None),
        row("star-right/anchors-the-first-label", &[allow("WebFetch(domain:example.*)")], "notexample.org", None),
        row("star-right/anchors-the-first-label-hyphenated", &[allow("WebFetch(domain:example.*)")], "evil-example.org", None),
        // A star inside a label, which is the general form of the same rule.
        row("star-inside/matches-within-one-label", &[allow("WebFetch(domain:*-lang.org)")], "rust-lang.org", Some(Allow)),
        row("star-inside/still-cannot-cross-a-dot", &[allow("WebFetch(domain:*-lang.org)")], "www.rust-lang.org", None),

        // -------------------------------------------------------------------
        // Lookalikes. Every host below is one somebody can register today; if
        // a row here starts matching, a user who approved `example.com` has
        // approved an attacker.
        // -------------------------------------------------------------------
        row("lookalike/hyphen-prefix", &[COM], "evil-example.com", None),
        row("lookalike/registered-underneath", &[COM], "example.com.attacker.net", None),
        row("lookalike/registered-underneath-a-cctld", &[COM], "example.com.evil.co.uk", None),
        row("lookalike/glued-prefix", &[COM], "notexample.com", None),
        row("lookalike/glued-suffix", &[COM], "example.common", None),
        row("lookalike/extra-tld-letter", &[COM], "example.como", None),
        row("lookalike/missing-tld-letter", &[COM], "example.co", None),
        row("lookalike/no-dot-at-all", &[COM], "examplecom", None),
        row("lookalike/digit-swap", &[COM], "examp1e.com", None),
        row("lookalike/punycode-homograph", &[COM], "xn--exmple-cua.com", None),
        // The same shape against the wildcard rule, because a `deny`-shaped
        // mistake and an `allow`-shaped one are the same matcher.
        row("lookalike/underneath-a-star-rule", &[allow("WebFetch(domain:*.example.com)")], "example.com.attacker.net", None),

        // -------------------------------------------------------------------
        // Case and the trailing dot. `example.com.` is a valid FQDN and the
        // same host; a matcher that disagreed would re-prompt for a host the
        // user approved a second ago. Both sides are normalised, so the rule
        // may be written either way too.
        // -------------------------------------------------------------------
        row("case/upper-host", &[COM], "EXAMPLE.COM", Some(Allow)),
        row("case/mixed-host", &[COM], "ExAmPlE.cOm", Some(Allow)),
        row("case/upper-rule", &[allow("WebFetch(domain:EXAMPLE.COM)")], "example.com", Some(Allow)),
        row("fqdn/trailing-dot-host", &[COM], "example.com.", Some(Allow)),
        row("fqdn/trailing-dot-rule", &[allow("WebFetch(domain:example.com.)")], "example.com", Some(Allow)),
        row("fqdn/trailing-dot-both-and-mixed-case", &[allow("WebFetch(domain:Example.Com.)")], "EXAMPLE.COM.", Some(Allow)),
        row("fqdn/trailing-dot-under-a-star-rule", &[allow("WebFetch(domain:*.example.com)")], "api.example.com.", Some(Allow)),
        // A trailing dot is stripped; an empty label **anywhere else** is not a
        // host and matches nothing, in any list.
        row("malformed/empty-label-in-the-middle", &[COM], "example..com", None),
        row("malformed/leading-dot", &[COM], ".example.com", None),
        // Asserted as it behaves. `trim_end_matches` strips *every* trailing
        // dot, not one, so `example.com..` — which is not a legal FQDN — is
        // read as `example.com`. Left alone rather than tightened: the same
        // normalisation runs in `NetworkTarget::new`, so the rule and the
        // session grant cannot disagree about it, and the extra dots do not
        // name a host anybody else can register. Tightening one side only
        // would be the real hazard.
        row("malformed/extra-trailing-dots-are-still-the-same-name", &[COM], "example.com..", Some(Allow)),
        row("malformed/empty-host", &[COM], "", None),
        row("malformed/just-a-dot", &[COM], ".", None),

        // -------------------------------------------------------------------
        // IDN. **Asserted as it behaves, not as one might wish.** The matcher
        // does no IDNA at all: it lowercases ASCII and compares labels. In
        // production that is survivable because `WebFetch` builds its host with
        // `Url::host_str`, which has already converted a Unicode host to
        // punycode — so the host arriving here is `xn--…` and a rule has to be
        // written the same way. A rule written in Unicode parses, is not
        // reported at boot, and matches nothing. That is a real trap, and it is
        // pinned here rather than papered over.
        // -------------------------------------------------------------------
        row("idn/punycode-rule-matches-punycode-host", &[allow("WebFetch(domain:xn--bcher-kva.example)")], "xn--bcher-kva.example", Some(Allow)),
        row("idn/a-unicode-rule-never-meets-a-punycode-host", &[allow("WebFetch(domain:b\u{fc}cher.example)")], "xn--bcher-kva.example", None),
        row("idn/a-unicode-rule-does-match-a-unicode-host", &[allow("WebFetch(domain:b\u{fc}cher.example)")], "b\u{fc}cher.example", Some(Allow)),
        // Case folding is ASCII-only, so a Unicode rule is case-sensitive in
        // the parts that are not ASCII. Documented, not desired.
        row("idn/case-folding-is-ascii-only", &[allow("WebFetch(domain:b\u{fc}cher.example)")], "B\u{dc}CHER.example", None),
        row("idn/punycode-is-all-ascii-so-it-case-folds", &[allow("WebFetch(domain:XN--BCHER-KVA.example)")], "xn--bcher-kva.example", Some(Allow)),

        // -------------------------------------------------------------------
        // IP literals and `localhost`. Egress to these is not egress to a name
        // — there is no registry, no TLD and no owner — and the matcher does
        // not know that. It splits on dots like anything else, which is worth
        // knowing before writing such a rule: a star spans an octet.
        // -------------------------------------------------------------------
        row("ip/exact", &[allow("WebFetch(domain:127.0.0.1)")], "127.0.0.1", Some(Allow)),
        row("ip/a-different-address-is-a-different-host", &[allow("WebFetch(domain:127.0.0.1)")], "127.0.0.2", None),
        row("ip/no-prefix-matching-on-an-octet", &[allow("WebFetch(domain:127.0.0.1)")], "127.0.0.10", None),
        row("ip/not-a-longer-address", &[allow("WebFetch(domain:127.0.0.1)")], "1127.0.0.1", None),
        // A star is a label and an octet is a label. `127.0.0.*` is a /24 and
        // reads like one; `*.0.0.1` is the same mechanism pointing the other
        // way and reads like nothing at all. Both are pinned so neither is a
        // surprise to whoever meets one in a settings file.
        row("ip/a-star-octet-is-a-slash-24", &[allow("WebFetch(domain:127.0.0.*)")], "127.0.0.9", Some(Allow)),
        row("ip/a-leading-star-spans-the-first-three-octets", &[allow("WebFetch(domain:*.0.0.1)")], "127.0.0.1", Some(Allow)),
        row("ip/a-leading-star-still-needs-the-tail", &[allow("WebFetch(domain:*.0.0.1)")], "127.0.0.2", None),
        // IPv6 arrives from `Url::host_str` in brackets, and the brackets are
        // part of the label. A rule written without them matches nothing.
        row("ip/v6-bracketed-as-the-url-crate-writes-it", &[allow("WebFetch(domain:[::1])")], "[::1]", Some(Allow)),
        row("ip/v6-an-unbracketed-rule-misses-a-bracketed-host", &[allow("WebFetch(domain:::1)")], "[::1]", None),
        row("localhost/exact", &[allow("WebFetch(domain:localhost)")], "localhost", Some(Allow)),
        row("localhost/is-not-a-suffix-of-a-real-domain", &[allow("WebFetch(domain:localhost)")], "localhost.attacker.net", None),
        row("localhost/a-star-rule-does-not-cover-the-bare-name", &[allow("WebFetch(domain:*.localhost)")], "localhost", None),

        // -------------------------------------------------------------------
        // Precedence, where a deny and an allow cover the same host — in both
        // orders, because "the deny happened to be written first" is not a
        // guarantee anybody should be resting on.
        // -------------------------------------------------------------------
        row("precedence/deny-then-allow", &[deny("WebFetch(domain:example.com)"), COM], "example.com", Some(Deny)),
        row("precedence/allow-then-deny", &[COM, deny("WebFetch(domain:example.com)")], "example.com", Some(Deny)),
        row("precedence/a-narrow-deny-beats-a-wide-allow", &[allow("WebFetch"), deny("WebFetch(domain:example.org)")], "example.org", Some(Deny)),
        row("precedence/a-wide-deny-beats-a-narrow-allow", &[deny("WebFetch"), allow("WebFetch(domain:example.net)")], "example.net", Some(Deny)),
        row("precedence/a-wildcard-deny-beats-an-exact-allow", &[deny("WebFetch(domain:*.example.com)"), allow("WebFetch(domain:api.example.com)")], "api.example.com", Some(Deny)),
        // …and a deny covers only what it names. A deny that swallowed the rest
        // of the allow list would look identical in every row above.
        row("precedence/a-narrow-deny-leaves-the-rest-of-the-allow-alone", &[allow("WebFetch"), deny("WebFetch(domain:example.org)")], "docs.rs", Some(Allow)),
        row("precedence/a-wildcard-deny-leaves-the-parent-alone", &[deny("WebFetch(domain:*.example.com)"), COM], "example.com", Some(Allow)),
        // Ask sits between them, in both directions.
        row("precedence/ask-beats-allow", &[allow("WebFetch(domain:example.net)"), ask("WebFetch(domain:example.net)")], "example.net", Some(Ask)),
        row("precedence/ask-loses-to-deny", &[ask("WebFetch"), deny("WebFetch(domain:docs.rs)"), allow("WebFetch")], "docs.rs", Some(Deny)),
        row("precedence/an-ask-wildcard-covers-a-subdomain", &[ask("WebFetch(domain:*.iana.org)"), allow("WebFetch")], "www.iana.org", Some(Ask)),

        // -------------------------------------------------------------------
        // Silence. `None` is what sends a call to the human, and it is the
        // verdict every row above that says `None` is really asserting.
        // -------------------------------------------------------------------
        row("silence/no-rules-at-all", &[], "example.com", None),
        row("silence/rules-about-other-hosts", &[COM, allow("WebFetch(domain:example.org)")], UNGRANTED, None),
        row("silence/a-domain-rule-for-another-tool", &[allow("WebSearch(domain:example.com)")], "example.com", None),
        row("silence/a-bare-rule-for-another-tool", &[allow("WebSearch")], "example.com", None),
        // A bare rule for *this* tool answers for every host, which is what
        // `[t]rust` writes and what makes it the bigger grant.
        row("wide/a-bare-tool-rule-covers-any-host", &[allow("WebFetch")], UNGRANTED, Some(Allow)),
        row("wide/domain-star-is-the-same-as-a-bare-rule", &[allow("WebFetch(domain:*)")], UNGRANTED, Some(Allow)),
    ];

    // Collected rather than asserted one at a time: a matcher that has widened
    // usually widens several rows at once, and the set of failures is what
    // names the invariant that went.
    let mut broken = Vec::new();
    for r in &matrix {
        match r.verdict() {
            Ok(got) if got == r.want => {}
            Ok(got) => broken.push(format!(
                "  {}: rules {:?} against `{}` gave {:?}, wanted {:?}",
                r.name, r.rules, r.host, got, r.want
            )),
            Err(why) => broken.push(format!("  {}: {why}", r.name)),
        }
    }
    assert!(
        broken.is_empty(),
        "{} of {} matrix rows broke:\n{}",
        broken.len(),
        matrix.len(),
        broken.join("\n")
    );

    // The table is the test, so an emptied table must not pass. This is what
    // fails if somebody ever "fixes" a red row by deleting it.
    assert!(matrix.len() >= 80, "the matrix lost rows: {}", matrix.len());
    let hosts: std::collections::BTreeSet<&str> = matrix.iter().map(|r| r.host).collect();
    assert!(hosts.len() >= 40, "the matrix lost hosts: {}", hosts.len());
}

#[test]
fn a_domain_rule_is_scoped_to_its_tool_across_all_six_hosts() {
    // The matrix asks about one tool. This is the other axis, over the same six
    // hosts: a rule naming `WebFetch` says nothing about `WebSearch`, and six
    // host grants do not add up to permission to run the tool for its own sake.
    let entries: Vec<PermissionEntry> = SIX
        .iter()
        .map(|h| PermissionEntry {
            rule: format!("WebFetch(domain:{h})"),
            kind: PermissionKind::Allow,
            source: PathBuf::from("settings.local.json"),
        })
        .collect();
    let (rules, notes) = Rules::parse(&entries);
    assert!(notes.is_empty(), "{notes:?}");
    for host in SIX {
        assert_eq!(
            rules.for_egress("WebFetch", host),
            Some(Decision::Allow),
            "{host} was written down and not honoured"
        );
        assert_eq!(
            rules.for_egress("WebSearch", host),
            None,
            "a grant naming WebFetch travelled to WebSearch for {host}"
        );
    }
    assert_eq!(rules.for_egress("WebFetch", UNGRANTED), None);
    assert_eq!(
        rules.for_tool("WebFetch"),
        None,
        "six host grants added up to permission to run the tool"
    );
}

// endregion: The matching matrix

// region: Six grants, one file, many runs
// ---------------------------------------------------------------------------
// Six grants, one file, many runs
//
// The matrix proves the matcher in one process against values in memory. What
// it cannot prove is that six grants, written one at a time by six separate
// runs into one JSON document, are still six distinct rules afterwards — that
// none overwrote another, that the file is still a file, and that the run after
// them all asks about none of the six and **still asks about a seventh**.
//
// That last one is the assertion this section exists for. A rule that grants
// too much passes every other check here.
// ---------------------------------------------------------------------------

/// One remembered grant, in a gate built from whatever is on disk right now —
/// which is what a fresh process does.
async fn remember_host(root: &Path, cwd: &Path, host: &'static str) {
    let (tool, calls) = TestTool::reaching("WebFetch", host);
    let out = run(
        root,
        cwd,
        registry(vec![tool]),
        &gate(root, vec![Answer::RememberNarrow]),
        &Fake::new(vec![
            call("WebFetch", json!({ "x": "1" })),
            text("ok.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;
    assert_eq!(out.ending, Ending::Done, "granting {host} did not finish");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "{host} was not reached");
}

/// Ask about one host with **nobody available to answer**, and report whether
/// the call ran. `true` means a rule allowed it; `false` means it was asked
/// about and, there being no answer, refused.
async fn allowed_without_asking(root: &Path, cwd: &Path, host: &'static str) -> bool {
    let (tool, calls) = TestTool::reaching("WebFetch", host);
    let out = run(
        root,
        cwd,
        registry(vec![tool]),
        &gate(root, Vec::new()),
        &Fake::new(vec![
            call("WebFetch", json!({ "x": "1" })),
            text("done.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;
    assert_eq!(out.ending, Ending::Done);
    calls.load(Ordering::SeqCst) == 1
}

#[tokio::test]
async fn six_hosts_remembered_one_at_a_time_all_survive_in_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = harness_dir(dir.path(), "{}");

    // The local file starts with keys that have nothing to do with permissions
    // and one `deny` that does. Six merging writes have to leave all of it
    // alone — this is the shape of the defect that has bitten this project
    // once, and six writes is six chances to repeat it.
    let before = json!({
        "statusLine": { "type": "command", "command": "hooks/status" },
        "env": { "EMMA_TEST": "1" },
        "permissions": { "deny": ["WebFetch(domain:evil.example)"] }
    });
    std::fs::write(
        file_for(&root),
        format!("{}\n", serde_json::to_string_pretty(&before).unwrap()),
    )
    .unwrap();

    // Six runs. Each builds its gate from the file the previous one left, so
    // nothing is carried between them in memory.
    for host in SIX {
        remember_host(&root, dir.path(), host).await;
    }

    // The file, as the user would find it: still JSON, still an object, still
    // carrying everything it started with.
    let raw = std::fs::read_to_string(file_for(&root)).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("six writes broke the JSON: {e}\n{raw}"));
    assert_eq!(doc["statusLine"]["command"], "hooks/status");
    assert_eq!(doc["env"]["EMMA_TEST"], "1");
    assert_eq!(
        doc["permissions"]["deny"][0],
        "WebFetch(domain:evil.example)"
    );

    // Six rules, one per host, in the order they were granted, and no seventh.
    let written: Vec<&str> = doc["permissions"]["allow"]
        .as_array()
        .expect("permissions.allow is an array")
        .iter()
        .map(|v| v.as_str().expect("a rule is a string"))
        .collect();
    let expected: Vec<String> = SIX
        .iter()
        .map(|h| format!("WebFetch(domain:{h})"))
        .collect();
    assert_eq!(
        written,
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "six grants did not survive as six rules"
    );
    // Stated separately from the equality above, because "none overwrote
    // another" is the claim and these two lines are what say it out loud.
    assert_eq!(written.len(), 6);
    assert_eq!(
        written
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        6,
        "two grants collapsed into one rule"
    );

    // A later run reads all six back and asks about none of them. The answer
    // queue is empty in every one of these, so a call that ran can only have
    // been allowed by the file.
    for host in SIX {
        assert!(
            allowed_without_asking(&root, dir.path(), host).await,
            "{host} was written down and the next run asked anyway"
        );
    }

    // **The assertion this section exists for.** A seventh host, never granted,
    // must still ask — and with nobody to answer, must not run. An over-broad
    // rule is invisible in every assertion above this line.
    assert!(
        !allowed_without_asking(&root, dir.path(), UNGRANTED).await,
        "a host nobody granted was allowed — a rule is matching more than it names"
    );
    // Two more that a widened matcher would let through: a subdomain of a
    // granted host, and a lookalike registered underneath one.
    assert!(
        !allowed_without_asking(&root, dir.path(), "www.example.com").await,
        "a bare-domain grant leaked to a subdomain"
    );
    assert!(
        !allowed_without_asking(&root, dir.path(), "example.com.attacker.net").await,
        "a grant leaked to a host registered underneath it"
    );
    // …and the `deny` that was in the file before any of this still fires.
    assert!(
        !allowed_without_asking(&root, dir.path(), "evil.example").await,
        "six merging writes lost the deny rule that was already there"
    );
}

#[tokio::test]
async fn a_wide_grant_is_the_only_thing_that_covers_a_host_nobody_named() {
    // The control for the test above. Same six hosts, same seventh — but the
    // grant is `[t]rust`, so the seventh is covered too. Without this, "the
    // seventh still asks" could be passing because the file is not being read
    // at all, and both tests would look identical from the outside.
    let dir = tempfile::tempdir().unwrap();
    let root = harness_dir(dir.path(), "{}");
    let (first, _) = TestTool::reaching("WebFetch", SIX[0]);
    run(
        &root,
        dir.path(),
        registry(vec![first]),
        &gate(&root, vec![Answer::RememberWide]),
        &Fake::new(vec![
            call("WebFetch", json!({ "x": "1" })),
            text("ok.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file_for(&root)).unwrap()).unwrap();
    assert_eq!(doc["permissions"]["allow"][0], "WebFetch");
    assert_eq!(doc["permissions"]["allow"].as_array().unwrap().len(), 1);

    for host in SIX {
        assert!(
            allowed_without_asking(&root, dir.path(), host).await,
            "{host}"
        );
    }
    assert!(allowed_without_asking(&root, dir.path(), UNGRANTED).await);
}

// endregion: Six grants, one file, many runs

// region: Live certification
// ---------------------------------------------------------------------------
// Live certification
//
// Everything above runs against a tool that counts calls. These two run against
// six real hosts over a real network, in real separate processes, with one real
// `settings.local.json` between them — because the claim being certified is
// about matching accuracy across runs, and a fake tool cannot fail to resolve.
//
// **Ignored by default**, and they must stay that way: `cargo test` runs on
// machines with no network, and a test whose result depends on DNS reports
// somebody else's outage as this crate's bug. They are driven from a shell, one
// process per host:
//
// ```text
//   EMMA_LIVE_DIR=<dir> EMMA_LIVE_HOST=example.com \
//     cargo test -p emma --test permissions live_grant -- --ignored --nocapture
//   …once per host…
//   EMMA_LIVE_DIR=<dir> \
//     cargo test -p emma --test permissions live_replay -- --ignored --nocapture
// ```
//
// With `EMMA_LIVE_DIR` unset they say so and pass, so a blanket
// `cargo test -- --ignored` cannot reach the network by accident.
//
// One request per host, ever. `Curl` runs once per allowed call and the driver
// runs each host once; there is no loop and no retry anywhere in here.
// ---------------------------------------------------------------------------

/// A tool that reaches one host and, when the gate lets it, **actually reaches
/// it** — a real DNS lookup, a real TLS handshake, a real HTTP response.
///
/// `curl` rather than an HTTP crate, because this test binary should not grow a
/// dependency to prove a point about permissions, and because a separate
/// process is the more honest demonstration: nothing in this file can fake a
/// 200 that did not happen.
struct Curl {
    host: &'static str,
    /// `HTTP <code> <host>` per call that ran, for the report.
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Tool for Curl {
    fn name(&self) -> &'static str {
        "WebFetch"
    }
    fn description(&self) -> &str {
        "fetches one URL, for real"
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "x": { "type": "string" } } })
    }
    fn meta(&self) -> ToolMeta {
        // The shape `WebFetch` really has: honestly read-only, and still how
        // bytes leave the machine. A gate that consulted only the write axis
        // would let every one of these through in silence.
        ToolMeta {
            read_only: true,
            reaches_network: true,
            idempotent: true,
        }
    }
    fn network_target(&self, _args: &Value) -> Option<NetworkTarget> {
        Some(NetworkTarget::new(
            self.host,
            format!("read https://{}/", self.host),
        ))
    }
    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        _args: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        let url = format!("https://{}/", self.host);
        let out = std::process::Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                "30",
                "-o",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
                "-w",
                "%{http_code}",
                &url,
            ])
            .output();
        let line = match out {
            Ok(o) if o.status.success() => format!(
                "HTTP {} {}",
                String::from_utf8_lossy(&o.stdout).trim(),
                self.host
            ),
            // An unreachable host is reported, never dropped: the point of six
            // hosts is six hosts, and quietly certifying five is the failure
            // this whole exercise is about.
            Ok(o) => format!(
                "UNREACHABLE {} — curl said: {}",
                self.host,
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => format!("UNREACHABLE {} — could not run curl: {e}", self.host),
        };
        println!("    {line}");
        self.log.lock().unwrap().push(line.clone());
        Ok(Ok(ToolOutcome::new(line)))
    }
}

fn live_dir() -> Option<PathBuf> {
    match std::env::var("EMMA_LIVE_DIR") {
        Ok(d) if !d.trim().is_empty() => Some(PathBuf::from(d)),
        _ => {
            println!("EMMA_LIVE_DIR is not set — skipping the live certification");
            None
        }
    }
}

/// The `.claude/` harness the live runs share. Created on first use, so the
/// driver does not have to.
fn live_root(dir: &Path) -> PathBuf {
    let root = dir.join(".claude");
    std::fs::create_dir_all(&root).unwrap();
    let settings = root.join("settings.json");
    if !settings.exists() {
        std::fs::write(&settings, "{}\n").unwrap();
    }
    root
}

/// Every host the live certification knows about, so `EMMA_LIVE_HOST` cannot
/// name something outside the matrix. Six granted, one never.
fn live_host(name: &str) -> &'static str {
    SIX.iter()
        .chain(std::iter::once(&UNGRANTED))
        .find(|h| **h == name)
        .copied()
        .unwrap_or_else(|| panic!("`{name}` is not one of the certified hosts"))
}

#[tokio::test]
#[ignore = "reaches a real host; see the region comment for how it is driven"]
async fn live_grant() {
    let Some(dir) = live_dir() else { return };
    let host = live_host(&std::env::var("EMMA_LIVE_HOST").expect("EMMA_LIVE_HOST"));
    let root = live_root(&dir);

    let log = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(Curl {
        host,
        log: log.clone(),
    });
    println!("  grant: {host}");
    let out = run(
        &root,
        &dir,
        registry(vec![tool]),
        &gate(&root, vec![Answer::RememberNarrow]),
        &Fake::new(vec![
            call("WebFetch", json!({ "x": "1" })),
            text("fetched.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;
    assert_eq!(out.ending, Ending::Done);

    let log = log.lock().unwrap();
    assert_eq!(log.len(), 1, "the call did not run exactly once: {log:?}");
    assert!(
        log[0].starts_with("HTTP 2") || log[0].starts_with("HTTP 3"),
        "{host} did not answer: {}",
        log[0]
    );
    let rule = format!("WebFetch(domain:{host})");
    let written = emma::permissions::already_written(&file_for(&root));
    assert!(
        written.contains(&rule),
        "`{rule}` was not written: {written:?}"
    );
}

#[tokio::test]
#[ignore = "reaches six real hosts; see the region comment for how it is driven"]
async fn live_replay() {
    let Some(dir) = live_dir() else { return };
    let root = live_root(&dir);

    // Every rule the file carries, printed, because the evidence for this test
    // is the file as much as the verdicts.
    let written = emma::permissions::already_written(&file_for(&root));
    println!("  {} rules on disk:", written.len());
    for rule in &written {
        println!("    {rule}");
    }
    assert_eq!(written.len(), 6, "six grants did not survive: {written:?}");

    // Six hosts, one empty answer queue each. Every fetch that happens is a
    // fetch a rule in that file allowed.
    for host in SIX {
        let log = Arc::new(Mutex::new(Vec::new()));
        let tool: Arc<dyn Tool> = Arc::new(Curl {
            host,
            log: log.clone(),
        });
        println!("  replay: {host}");
        let out = run(
            &root,
            &dir,
            registry(vec![tool]),
            &gate(&root, Vec::new()),
            &Fake::new(vec![
                call("WebFetch", json!({ "x": "1" })),
                text("fetched.\n\nGOAL COMPLETE"),
            ]),
        )
        .await;
        assert_eq!(out.ending, Ending::Done);
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1, "{host} was asked about again: {log:?}");
        assert!(
            log[0].starts_with("HTTP 2") || log[0].starts_with("HTTP 3"),
            "{host} did not answer: {}",
            log[0]
        );
    }

    // The seventh. Nobody granted it, nobody is there to approve it, and it
    // must not be reached — over a real network, where a widened rule shows up
    // as a real request to a host the user never named.
    let log = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(Curl {
        host: UNGRANTED,
        log: log.clone(),
    });
    println!("  replay: {UNGRANTED} (must not be reached)");
    run(
        &root,
        &dir,
        registry(vec![tool]),
        &gate(&root, Vec::new()),
        &Fake::new(vec![
            call("WebFetch", json!({ "x": "1" })),
            text("refused.\n\nGOAL COMPLETE"),
        ]),
    )
    .await;
    assert!(
        log.lock().unwrap().is_empty(),
        "a host nobody granted was contacted for real"
    );
    println!("    not contacted");
}

// endregion: Live certification
