//! Mechanical only. Nothing here starts the real `claude`.
//!
//! The stream tests run against `tests/fixtures/claude-engine/stream.jsonl`.
//! **That file is synthetic**: it was written from the documented `stream-json`
//! shape, event by event, not captured from a session. A capture would have
//! carried a machine's paths, its connected MCP servers, its skill inventory and
//! a real session id into the repository, and none of those belong here.
//!
//! **What that costs, plainly.** A fixture somebody wrote agrees with its
//! author. These tests therefore prove that the parser handles the shape this
//! build believes in — not that the shape is right. The mitigations are the two
//! this module was designed around: nothing in the stream is required to parse
//! (an unmodelled line becomes [`Event::Unknown`] and reaches the screen and the
//! log whole), and the `claude_code_version` that produced a stream is recorded
//! beside it. The version in the fixture, `2.1.263`, is the one on the machine
//! where it was written, read with `claude --version`; the shapes are the ones
//! the fork's own capture showed, transcribed rather than copied.
//!
//! What would settle it: run a real `claude -p --output-format stream-json
//! --verbose` against a scratch directory, diff its event *shapes* (not its
//! content) against this file, and move the chip.

use super::*;

/// Built component by component rather than from one slash-separated string, so
/// the path uses this platform's separator. `cmd.exe` reads a forward slash as
/// the start of a switch, so `type "…/fixtures/…"` fails — and, measured on this
/// box, fails while exiting 0, which is exactly the silent-failure shape these
/// tests exist to catch.
fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude-engine")
        .join("stream.jsonl")
}

fn fixture() -> Vec<String> {
    let path = fixture_path();
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the stream fixture is missing at {path:?}: {e}"))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

fn events() -> Vec<Event> {
    fixture().iter().map(|l| Event::parse(l)).collect()
}

// region: The stream

/// The whole fixture, parsed. Nothing in it may land in `Unknown`, because
/// `Unknown` is the arm for a *future* format, not for the one this build models.
#[test]
fn every_fixture_line_parses_into_a_known_event() {
    for (line, event) in fixture().iter().zip(events()) {
        assert!(
            !matches!(event, Event::Unknown(_)),
            "a fixture line fell through to Unknown: {}",
            clip(line, 200)
        );
    }
    // And every arm this module documents is exercised, so a change that broke
    // one of them could not pass by simply not being covered.
    let seen: Vec<&str> = events()
        .iter()
        .map(|e| match e {
            Event::Init(_) => "init",
            Event::Assistant { .. } => "assistant",
            Event::ToolResults(_) => "results",
            Event::PermissionDenied { .. } => "denied",
            Event::Result(_) => "result",
            Event::Ignored(_) => "ignored",
            Event::Unknown(_) => "unknown",
        })
        .collect();
    for want in [
        "init",
        "assistant",
        "results",
        "denied",
        "result",
        "ignored",
    ] {
        assert!(
            seen.contains(&want),
            "the fixture never exercises {want}: {seen:?}"
        );
    }
}

/// The four facts the header quotes, read off the init event.
#[test]
fn the_init_event_carries_the_facts_the_header_states() {
    let init = events()
        .into_iter()
        .find_map(|e| match e {
            Event::Init(i) => Some(i),
            _ => None,
        })
        .expect("the stream opens with an init event");
    assert_eq!(init.permission_mode, "default");
    assert_eq!(init.version, "2.1.263");
    assert!(init.model.starts_with("claude-"), "{}", init.model);
    assert!(!init.session_id.is_empty());

    let line = header(&init);
    assert!(line.contains("2.1.263"), "{line}");
    assert!(line.contains("default"), "{line}");
    // The honest-limits half. A header that named the version and stopped would
    // leave the user believing Emma's gate was still in front of the work.
    assert!(line.contains("claude's own rules, not Emma's"), "{line}");
    assert!(line.contains("approval prompts do not apply"), "{line}");
}

/// No personal path, no real identifier, no machine of anybody's, anywhere in
/// the fixture. Ruling 5, pinned rather than remembered: a fixture is exactly
/// the file somebody regenerates from a real session in a hurry.
#[test]
fn the_fixture_carries_no_identifier_and_no_real_machines_paths() {
    let text = std::fs::read_to_string(fixture_path()).unwrap();
    for bad in [
        "/Users/",
        "C:\\Users",
        "/home/",
        "@gmail",
        "192.168.",
        "10.0.0.",
        "/private/tmp",
        "cc-socks",
    ] {
        assert!(
            !text.contains(bad),
            "the fixture carries `{bad}`, which names somebody's machine"
        );
    }
    // And it is LF-only, so the parser is never tested against line endings the
    // repository would rewrite on the next checkout.
    assert!(!text.contains('\r'), "the fixture has CRLF line endings");
}

/// A line this build has never seen is kept and shown, not dropped and not
/// fatal. The format has no stability contract; this arm is the whole mitigation.
#[test]
fn an_unknown_event_type_is_kept_and_shown_rather_than_dropped() {
    let term = Term::recording();
    let log = SessionLog::none();
    let mut run = Run::default();

    let line = r#"{"type":"telepathy","subtype":"nobody_wrote_this_yet","payload":42}"#;
    let event = Event::parse(line);
    assert_eq!(event, Event::Unknown(line.to_string()));
    apply(&mut run, event, &term, &log);
    assert_eq!(run.unknown_events, 1);

    // Not JSON at all, which is what a crash message on stdout looks like.
    let mut run = Run::default();
    apply(&mut run, Event::parse("Segmentation fault"), &term, &log);
    assert_eq!(run.unknown_events, 1);
}

/// The two events that are understood and deliberately silent, named so a
/// change that started painting them fails here.
#[test]
fn account_and_thinking_estimates_are_ignored_by_name() {
    let ignored: Vec<&str> = events()
        .iter()
        .filter_map(|e| match e {
            Event::Ignored(what) => Some(*what),
            _ => None,
        })
        .collect();
    assert!(ignored.contains(&"rate_limit_event"), "{ignored:?}");
    assert!(ignored.contains(&"thinking_tokens"), "{ignored:?}");
}

/// A tool the model called becomes the same transcript block the Emma loop
/// paints, and the result that follows is attributed back to it by id.
#[test]
fn a_tool_use_block_becomes_a_tool_started_row_and_its_result_is_attributed() {
    let term = Term::recording();
    let log = SessionLog::none();
    let mut run = Run::default();
    for event in events() {
        apply(&mut run, event, &term, &log);
    }
    assert_eq!(run.tool_calls, 2, "the stream calls Read then Write");
    assert!(
        run.names.values().any(|n| n == "Read"),
        "the Read call was not recorded: {:?}",
        run.names
    );
}

/// The event this engine exists to make loud. A denial is not a tool failure and
/// must not be painted as one: no answer at the keyboard could have allowed it.
#[test]
fn a_denied_tool_becomes_a_blocked_row_and_not_a_failed_one() {
    let denial = events()
        .into_iter()
        .find_map(|e| match e {
            Event::PermissionDenied {
                tool,
                tool_use_id,
                message,
            } => Some((tool, tool_use_id, message)),
            _ => None,
        })
        .expect("the stream contains a permission denial");
    assert_eq!(denial.0, "Write");
    assert!(denial.1.starts_with("toolu_"), "{}", denial.1);
    assert!(denial.2.contains("haven't granted"), "{}", denial.2);

    let term = Term::recording();
    let log = SessionLog::none();
    let mut run = Run::default();
    for event in events() {
        apply(&mut run, event, &term, &log);
    }
    assert_eq!(run.denials, 1);
}

/// The four counts and the price, off the result event.
#[test]
fn the_result_event_yields_the_four_usage_counts_and_the_cost() {
    let f = events()
        .into_iter()
        .find_map(|e| match e {
            Event::Result(f) => Some(f),
            _ => None,
        })
        .expect("every run ends with one result event");
    assert_eq!(f.stop_reason, "end_turn");
    assert!(!f.is_error);
    assert!(f.cost_usd.unwrap() > 0.0);
    assert!(f.usage.output_tokens > 0);
    assert!(f.usage.cache_read_input_tokens > 0);
    // Never invented. The stream reports no context window and this must stay
    // zero rather than being filled with the nearest plausible number.
    assert_eq!(f.usage.context_window, 0);
    // Nor is the CLI's own tool use provider-side work Emma was billed for.
    assert_eq!(f.usage.server_tool_use, Default::default());
}

/// Usage on an `assistant` line is per request and repeats across the messages
/// of one turn, so summing it overcounts. The result event is the only source.
#[test]
fn usage_is_read_from_the_result_and_never_summed_from_assistant_lines() {
    let term = Term::recording();
    let log = SessionLog::none();
    let mut run = Run::default();
    for event in events() {
        apply(&mut run, event, &term, &log);
    }
    let f = run.finished.expect("the stream finishes");
    // Three assistant lines carry 4096 cache-creation tokens each and a fourth
    // carries 336. A parser that summed them would report 12,624, not this.
    assert_eq!(f.usage.cache_creation_input_tokens, 4432);
}

// endregion: The stream

// region: The flags, and the rule that binds

/// **The most important test in this change.** Emma may hand the child the
/// posture the user handed Emma and never more, so a run started without
/// `--allow-all` must not contain the bypass under any spelling.
#[test]
fn without_allow_all_emma_never_passes_dangerously_skip_permissions() {
    let a = args(&Flags {
        goal: "fix the build".into(),
        model: None,
        allow_all: false,
    });
    assert!(
        !a.iter().any(|f| f.contains("skip-permissions")),
        "the bypass leaked into a run that was never given it: {a:?}"
    );
    assert!(
        !a.iter().any(|f| f == "bypassPermissions"),
        "the bypass leaked in under its permission-mode spelling: {a:?}"
    );
    // Passed positively, so a `defaultMode` somebody wrote in a settings file
    // cannot widen a run Emma launched. Omitting the flag would leave that door
    // open and would look identical here.
    let i = a
        .iter()
        .position(|f| f == "--permission-mode")
        .unwrap_or_else(|| panic!("the default mode must be stated rather than assumed: {a:?}"));
    assert_eq!(a[i + 1], "default");
}

/// And the other half: given the bypass, Emma passes it through rather than
/// quietly running the child at a narrower posture than the user chose.
#[test]
fn allow_all_passes_the_bypass_through_and_only_then() {
    let a = args(&Flags {
        goal: "fix the build".into(),
        model: None,
        allow_all: true,
    });
    assert!(
        a.iter().any(|f| f == "--dangerously-skip-permissions"),
        "{a:?}"
    );
    assert!(
        !a.iter().any(|f| f == "--permission-mode"),
        "two permission postures on one command line: {a:?}"
    );
}

/// The shape every run has, whatever else is set. `--verbose` is not decoration:
/// the CLI refuses `stream-json` in print mode without it.
#[test]
fn every_invocation_is_print_stream_json_and_verbose() {
    let a = args(&Flags {
        goal: "hello".into(),
        model: None,
        allow_all: false,
    });
    assert_eq!(a[0], "-p");
    assert_eq!(
        a[1], "hello",
        "the goal is one argument, never shell-joined"
    );
    let i = a.iter().position(|f| f == "--output-format").unwrap();
    assert_eq!(a[i + 1], "stream-json");
    assert!(a.iter().any(|f| f == "--verbose"), "{a:?}");
}

/// Emma's model setting reaches the child, and its absence means the child's own
/// default rather than a guess made here.
#[test]
fn the_model_is_passed_through_when_one_is_resolved() {
    let with = args(&Flags {
        goal: "x".into(),
        model: Some("opus".into()),
        allow_all: false,
    });
    let i = with
        .iter()
        .position(|f| f == "--model")
        .expect("model flag");
    assert_eq!(with[i + 1], "opus");

    let without = args(&Flags {
        goal: "x".into(),
        model: None,
        allow_all: false,
    });
    assert!(!without.iter().any(|f| f == "--model"), "{without:?}");
}

// endregion: The flags, and the rule that binds

// region: Records and endings

/// The records a claude goal writes fold back into a conversation, which is what
/// keeps `--resume`, the harness pages and `export-training` working without any
/// of them learning that a second engine exists.
#[test]
fn a_claude_goal_folds_back_into_a_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionLog::open(dir.path(), "sess-test").unwrap();
    let term = Term::recording();
    log.append(
        "goal",
        json!({"text": "read a.txt", "opening": "read a.txt"}),
    );
    let mut run = Run::default();
    for event in events() {
        apply(&mut run, event, &term, &log);
    }

    let messages = crate::session::fold(&log.path()).unwrap();
    assert!(
        messages.len() > 1,
        "a claude goal folded to nothing: {messages:?}"
    );
    let json = serde_json::to_string(&messages).unwrap();
    // The tool use survived as a tool use rather than as an opaque passthrough,
    // which is what `raw_content`'s one normalisation is for: a passthrough here
    // would mean a resumed conversation whose tool calls have no results paired
    // to them, and the API rejects that.
    assert!(json.contains("tool_use"), "{json}");
    assert!(json.contains("tool_result"), "{json}");
    // The thinking block's signature came from the model and is re-sent
    // verbatim; a rebuilt one is rejected on the next call.
    assert!(json.contains("signature"), "{json}");
}

/// The CLI splits one assistant message across several stdout lines, each
/// carrying the same `message.id`. Recording one turn per line would put
/// consecutive assistant messages in the transcript, which no API accepts back,
/// so the record is buffered by that id.
#[test]
fn one_message_split_across_lines_becomes_one_assistant_record() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionLog::open(dir.path(), "sess-merge").unwrap();
    let term = Term::recording();
    let mut run = Run::default();
    for event in events() {
        apply(&mut run, event, &term, &log);
    }
    let records = SessionLog::read(&log.path()).unwrap();
    let assistants: Vec<&Value> = records
        .iter()
        .filter(|r| r["kind"] == "assistant")
        .collect();
    // The stream is two requests: one that thought and called Read, then wrote
    // and was refused, and one that answered. Four assistant lines collapse into
    // three records, and the first two of those share one `message.id`.
    assert_eq!(
        assistants.len(),
        3,
        "one record per stdout line rather than per message: {assistants:#?}"
    );
    let first = &assistants[0]["raw_content"];
    let kinds: Vec<&str> = first
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["thinking", "tool_use"], "{first}");

    // And no two assistant messages end up adjacent in the folded conversation.
    let messages = crate::session::fold(&log.path()).unwrap();
    for pair in messages.windows(2) {
        assert!(
            !(pair[0].role == emma_llm::Role::Assistant
                && pair[1].role == emma_llm::Role::Assistant),
            "two assistant messages in a row would be rejected on resume: {messages:?}"
        );
    }
}

/// The record that says which engine ran, so a transcript read six months later
/// does not have to be inferred from the shape of its tool calls.
#[test]
fn the_engine_record_names_the_engine_and_the_permission_mode() {
    let dir = tempfile::tempdir().unwrap();
    let log = SessionLog::open(dir.path(), "sess-engine").unwrap();
    let term = Term::recording();
    let mut run = Run::default();
    for event in events() {
        apply(&mut run, event, &term, &log);
    }
    let records = SessionLog::read(&log.path()).unwrap();
    let engine = records
        .iter()
        .find(|r| r["kind"] == "engine")
        .expect("every claude goal records which engine ran");
    assert_eq!(engine["engine"], "claude");
    assert_eq!(engine["permission_mode"], "default");
    assert_eq!(engine["version"], "2.1.263");
    // The child's own session id, so a run can be found in claude's transcripts
    // as well as in Emma's.
    assert!(engine["claude_session_id"].as_str().unwrap().len() > 10);

    // And the denial is a record of its own, not only a line on a screen that
    // scrolled away.
    let blocked = records
        .iter()
        .find(|r| r["kind"] == "tool_blocked")
        .expect("a denial is recorded");
    assert_eq!(blocked["tool"], "Write");
}

/// A run whose work was refused reported `end_turn`, because the model did stop
/// talking. It is not a completed goal and must not be shown as one.
#[test]
fn a_run_with_a_denial_does_not_end_done() {
    let term = Term::recording();
    let log = SessionLog::none();
    let mut run = Run::default();
    for event in events() {
        apply(&mut run, event, &term, &log);
    }
    assert_eq!(run.finished.as_ref().unwrap().stop_reason, "end_turn");
    assert_eq!(
        ending_of(&run),
        Ending::Stalled,
        "a goal whose tools were refused was reported as finished"
    );

    // The same stream without the denial is a completed goal.
    let mut clean = Run::default();
    for event in events() {
        if matches!(event, Event::PermissionDenied { .. }) {
            continue;
        }
        apply(&mut clean, event, &term, &log);
    }
    assert_eq!(ending_of(&clean), Ending::Done);
}

/// A stream that stopped before its result event did not finish, whatever the
/// last thing on screen looked like.
#[test]
fn a_stream_that_never_reported_a_result_is_not_a_finished_goal() {
    let run = Run::default();
    assert!(matches!(ending_of(&run), Ending::Provider(_)));
}

// endregion: Records and endings

// region: Interruption, behind fakes

/// A child that answers the interrupt. `exited` flips once it is delivered,
/// which is what a program that writes its ending and leaves looks like.
#[derive(Default)]
struct Obedient {
    sent: std::sync::Mutex<Vec<&'static str>>,
    gone: std::sync::atomic::AtomicBool,
}

impl Signalling for Obedient {
    fn interrupt(&self) -> Result<(), String> {
        self.sent.lock().unwrap().push("interrupt");
        self.gone.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn kill(&self) -> Result<(), String> {
        self.sent.lock().unwrap().push("kill");
        Ok(())
    }
    fn exited(&self) -> bool {
        self.gone.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// One that does not.
#[derive(Default)]
struct Stubborn {
    sent: std::sync::Mutex<Vec<&'static str>>,
}

impl Signalling for Stubborn {
    fn interrupt(&self) -> Result<(), String> {
        self.sent.lock().unwrap().push("interrupt");
        Ok(())
    }
    fn kill(&self) -> Result<(), String> {
        self.sent.lock().unwrap().push("kill");
        Ok(())
    }
    fn exited(&self) -> bool {
        false
    }
}

/// A platform with no interrupt at all — which is Windows, and is why this fake
/// exists rather than a `cfg`.
#[derive(Default)]
struct Uninterruptible {
    sent: std::sync::Mutex<Vec<&'static str>>,
    gone: std::sync::atomic::AtomicBool,
}

impl Signalling for Uninterruptible {
    fn interrupt(&self) -> Result<(), String> {
        Err("no interrupt on this platform".into())
    }
    fn kill(&self) -> Result<(), String> {
        self.sent.lock().unwrap().push("kill");
        self.gone.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn exited(&self) -> bool {
        self.gone.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The interrupt first, always. The reason to interrupt rather than kill is that
/// the child gets to write its `result` event, which is the only place the run's
/// real cost is reported.
#[tokio::test]
async fn a_child_that_exits_on_the_interrupt_is_never_killed() {
    let child = Obedient::default();
    let what = stop(&child, Duration::from_millis(200), Duration::from_millis(5)).await;
    assert_eq!(what, Stopped::Interrupted);
    assert_eq!(*child.sent.lock().unwrap(), vec!["interrupt"]);
}

/// And a child that ignores it does not get to hold the session open.
#[tokio::test]
async fn an_interrupt_comes_first_and_the_kill_only_after_the_grace() {
    let child = Stubborn::default();
    let what = stop(&child, Duration::from_millis(60), Duration::from_millis(5)).await;
    assert_eq!(what, Stopped::Killed);
    assert_eq!(*child.sent.lock().unwrap(), vec!["interrupt", "kill"]);
}

/// Nothing is signalled at a process that has already gone. The recycled-pid
/// hazard `runctl` was written around applies here for the same reason.
#[tokio::test]
async fn a_child_that_already_exited_is_never_signalled() {
    let child = Obedient::default();
    child.gone.store(true, std::sync::atomic::Ordering::SeqCst);
    let what = stop(&child, Duration::from_millis(60), Duration::from_millis(5)).await;
    assert_eq!(what, Stopped::Already);
    assert!(
        child.sent.lock().unwrap().is_empty(),
        "a dead child was signalled"
    );
}

/// **The Windows case, and the defect this port exists to not reintroduce.**
/// A platform that cannot deliver an interrupt must still end the child, must
/// not sit out the grace period waiting for a request it never sent, and must
/// not tell the user its ending was written.
#[tokio::test]
async fn a_platform_without_an_interrupt_kills_at_once_and_says_so() {
    let child = Uninterruptible::default();
    let began = std::time::Instant::now();
    let what = stop(&child, Duration::from_secs(30), Duration::from_millis(5)).await;
    assert_eq!(what, Stopped::KilledWithoutInterrupt);
    assert_eq!(*child.sent.lock().unwrap(), vec!["kill"]);
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "the stop waited out a grace period for an interrupt it could not send"
    );

    let words = notice(what, Duration::from_secs(5));
    assert!(words.contains("cannot interrupt"), "{words}");
    assert!(words.contains("cost report is missing"), "{words}");
    // The wording the fork shipped, which was wrong twice over on Windows: it
    // named a signal that was never sent and left the reader believing the child
    // might still be running when it had just been killed.
    assert!(!words.contains("may still be running"), "{words}");
}

/// Every notice is a true sentence about what happened, and no notice claims an
/// ending was written unless one was asked for.
#[test]
fn no_notice_claims_the_child_wrote_an_ending_it_was_never_asked_for() {
    let grace = Duration::from_secs(5);
    assert!(notice(Stopped::Already, grace).contains("already exited"));
    assert!(notice(Stopped::Interrupted, grace).contains("wrote its ending"));
    assert!(!notice(Stopped::Killed, grace).contains("wrote its ending"));
    assert!(!notice(Stopped::KilledWithoutInterrupt, grace).contains("wrote its ending"));
    assert!(notice(Stopped::Failed, grace).contains("may still be running"));
    assert!(notice(Stopped::Killed, grace).contains("5s"));
}

// endregion: Interruption, behind fakes

// region: A real child, on this platform

/// A command that runs long enough to be caught alive, on whichever platform
/// this is. Both arms are real programs, not a `cfg` hole: the whole point of
/// these three tests is that the *operating system* answers.
fn long_runner() -> tokio::process::Command {
    #[cfg(windows)]
    {
        // `ping` is on every Windows and takes about a second per count, so this
        // is roughly thirty seconds of a live process. Spawned directly rather
        // than through `cmd`, so the pid we hold is the pid that must die.
        let mut c = tokio::process::Command::new("ping");
        c.args(["-n", "30", "127.0.0.1"]);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = tokio::process::Command::new("sleep");
        c.arg("30");
        c
    }
}

/// **The regression class, against the real operating system.** A kill that
/// reports success over a running command is the defect this repository has paid
/// for; the fork's `exited()` answered `true` on Windows without asking anybody,
/// so every stop reported "claude had already exited" over a child that was
/// still working. Nothing here is faked: a real process is spawned, asked
/// whether it has exited, stopped, and asked again.
#[tokio::test]
async fn a_live_child_is_never_reported_gone_and_a_stop_really_ends_it() {
    let mut child = long_runner()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("a long-running child");
    let pid = child.id().expect("a freshly spawned child has a pid");
    let signals = Spawned::of(pid).expect("a handle on a child we just spawned");

    assert!(
        !signals.exited(),
        "a child that is still running reported itself gone — this is the \
         kill-reports-success defect"
    );

    let what = stop(
        &signals,
        Duration::from_millis(300),
        Duration::from_millis(10),
    )
    .await;
    assert!(
        matches!(
            what,
            Stopped::Interrupted | Stopped::Killed | Stopped::KilledWithoutInterrupt
        ),
        "a live child was not stopped: {what:?}"
    );

    // Reaped, so a Unix zombie is not mistaken for a live process, and then
    // asked again. This is the assertion that would have caught the fork.
    let _ = child.wait().await;
    assert!(
        signals.exited(),
        "the child outlived the stop that claimed to have ended it"
    );
}

/// And the other end of it: a child that has already been reaped is reported
/// gone, so the stop does not signal at a pid that no longer belongs to it.
#[tokio::test]
async fn a_reaped_child_is_reported_gone_and_is_never_signalled() {
    let mut child = long_runner()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("a long-running child");
    let pid = child.id().expect("a pid");
    let signals = Spawned::of(pid).expect("a handle");
    child.kill().await.expect("killed");
    let _ = child.wait().await;

    assert!(signals.exited(), "a reaped child still reads as running");
    assert_eq!(
        stop(
            &signals,
            Duration::from_millis(50),
            Duration::from_millis(5)
        )
        .await,
        Stopped::Already
    );
}

/// The resolver finds a program that is certainly installed, and finds it as a
/// full path rather than a bare name — which is the whole point on Windows,
/// where `std::process::Command` does not consult `PATHEXT`.
#[test]
fn the_resolver_finds_a_real_program_on_this_machine_as_a_full_path() {
    #[cfg(windows)]
    let (name, want_ext) = ("cmd", "exe");
    #[cfg(not(windows))]
    let (name, want_ext) = ("sh", "");

    let found = resolve(name).unwrap_or_else(|| panic!("`{name}` is not on PATH"));
    assert!(
        found.program.is_absolute() || found.program.components().count() > 1,
        "the resolver returned a bare name: {:?}",
        found.program
    );
    assert!(found.program.is_file(), "{:?}", found.program);
    assert!(
        !found.shim,
        "{name} is a real executable, not a script shim"
    );
    if !want_ext.is_empty() {
        assert_eq!(
            found
                .program
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some(want_ext)
        );
    }
}

/// A name nobody has installed is `None`, and `missing_cli` turns that into a
/// sentence rather than a panic.
#[test]
fn a_program_that_is_not_installed_resolves_to_nothing() {
    assert!(resolve("emma-no-such-program-4a87fb").is_none());
}

// endregion: A real child, on this platform

// region: The Windows shim

/// The extension order, and which of them the kernel refuses to exec.
///
/// A real `claude.exe` must beat a `claude.cmd` beside it, because the shim is
/// slower and drags `cmd.exe`'s parsing rules in with it.
#[cfg(windows)]
#[test]
fn a_real_executable_beats_a_script_shim_of_the_same_name() {
    let c = candidates("claude");
    let names: Vec<&str> = c.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec!["claude.exe", "claude.com", "claude.cmd", "claude.bat"]
    );
    assert!(!c[0].1, "an .exe is not a shim");
    assert!(c[2].1 && c[3].1, "a .cmd and a .bat are shims: {c:?}");
}

/// **The measured one.** Every character `cmd.exe` acts on is neutralised,
/// including the quotes — because if the quotes reach `cmd` unescaped it enters
/// quoted mode, and inside quoted mode `%NAME%` is still expanded. A goal saying
/// `%USERPROFILE%` would then have the user's home directory spliced into the
/// prompt before the model saw it.
///
/// Certified against the real `cmd.exe` on Windows 11 on 2026-09-06, with a
/// shim that dumps its argv: with this escaping all ten hostile shapes round
/// trip; without the quote escaping, `pct %PATH% pct` arrived as the machine's
/// whole `PATH`. The end-to-end half of that measurement is
/// `the_shim_path_hands_the_child_its_goal_verbatim` below.
#[cfg(windows)]
#[test]
fn every_character_cmd_would_act_on_is_escaped_in_the_shim_command_line() {
    let line = shim_command_line(
        std::path::Path::new("C:/src/tools/claude.cmd"),
        &[
            "-p".to_string(),
            "amp & pct %PATH% pipe | bang !x!".to_string(),
        ],
    )
    .unwrap();
    assert_eq!(
        line,
        "/C ^\"C:/src/tools/claude.cmd^\" ^\"-p^\" ^\"amp ^& pct ^%PATH^% pipe ^| bang ^!x^!^\""
    );
    // And the invariant behind that string, so a later change to the quoting is
    // held to the rule rather than to one example: reading the line as cmd does,
    // a caret consumes the character after it, and nothing cmd acts on is ever
    // left over.
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '^' {
            chars.next();
            continue;
        }
        assert!(
            !CMD_META.contains(&c),
            "`{c}` reached cmd unescaped in: {line}"
        );
    }
}

/// A newline cannot be carried through `cmd.exe` and is refused rather than
/// truncated. Measured: `cmd` ends its command line at the first CR or LF, runs
/// what it has, and exits 0 — silent loss, which is one of the eight regression
/// classes by name.
#[cfg(windows)]
#[test]
fn a_multi_line_goal_through_a_shim_is_refused_rather_than_cut() {
    let err = shim_command_line(
        std::path::Path::new("C:/src/tools/claude.cmd"),
        &["-p".to_string(), "line one\nline two".to_string()],
    )
    .expect_err("a two-line goal through cmd.exe would be silently cut");
    assert!(err.contains("silently cut"), "{err}");
    assert!(err.contains("claude.exe"), "the remedy is not named: {err}");

    // And a one-line goal is not refused, so the check is about newlines and not
    // about length or punctuation.
    assert!(shim_command_line(
        std::path::Path::new("C:/src/tools/claude.cmd"),
        &["-p".to_string(), "line one line two".to_string()],
    )
    .is_ok());
}

// endregion: The Windows shim

// region: The whole engine, over a fake child

/// A stand-in for the CLI: a real program, on this platform, that prints the
/// fixture and records the goal it was handed.
///
/// On Windows it is a `.cmd`, which means the whole shim path — `cmd.exe`, the
/// caret escaping, the raw command line — is what these tests exercise. On Unix
/// it is a shell script spawned directly. Both arms are real; neither is a
/// `cfg` that returns a convenient answer.
fn fake_cli(dir: &std::path::Path) -> (Launch, std::path::PathBuf) {
    let argv = dir.join("argv.txt");
    let fixture = fixture_path();
    #[cfg(windows)]
    {
        let script = dir.join("claude.cmd");
        // Delayed expansion, so the goal is substituted after parsing and its
        // metacharacters are never re-read as syntax. `%~2` is the goal: the
        // argument list is `-p <goal> …`.
        let body = format!(
            "@echo off\r\n\
             setlocal enabledelayedexpansion\r\n\
             set \"GOAL=%~2\"\r\n\
             > \"{argv}\" echo(!GOAL!\r\n\
             type \"{fixture}\"\r\n",
            argv = argv.display(),
            fixture = fixture.display(),
        );
        std::fs::write(&script, body).unwrap();
        (
            Launch {
                program: script,
                shim: true,
            },
            argv,
        )
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("claude.sh");
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$2\" > '{argv}'\ncat '{fixture}'\n",
            argv = argv.display(),
            fixture = fixture.display(),
        );
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (
            Launch {
                program: script,
                shim: false,
            },
            argv,
        )
    }
}

fn handoff(dir: &std::path::Path, cli: Launch) -> Handoff {
    Handoff {
        cwd: dir.to_path_buf(),
        model: None,
        allow_all: false,
        timeout: Duration::from_secs(30),
        interrupt: Interrupt::new(),
        log: Arc::new(SessionLog::open(dir, "sess-drive").unwrap()),
        term: Arc::new(Term::recording()),
        session_id: "sess-drive".to_string(),
        spend: Spend::new(),
        cli,
    }
}

/// The whole engine, end to end, over a child this test really spawned: the
/// command line is built, a process runs, its stdout is read line by line, and
/// the turns come back decoded and recorded.
#[tokio::test]
async fn the_engine_drives_a_real_child_and_decodes_every_turn_it_prints() {
    let dir = tempfile::tempdir().unwrap();
    let (cli, _) = fake_cli(dir.path());
    let h = handoff(dir.path(), cli);
    let outcome = run_goal(&h, "read a.txt and write b.txt").await;

    // Denied a tool, so not a finished goal however the child reported it.
    assert_eq!(outcome.ending, Ending::Stalled, "{:?}", outcome.ending);
    assert!(
        outcome.text.contains("waiting for permission"),
        "the last prose turn did not reach the outcome: {:?}",
        outcome.text
    );
    assert_eq!(outcome.iterations, 1);
    assert_eq!(outcome.kicks, 0);
    // Weighted by the caller's function, over the *result* event's counts:
    // 20 + 4432*5/4 + 24576/10 + 311 = 8328.
    assert_eq!(outcome.tokens, 8328);

    let records = SessionLog::read(&h.log.path()).unwrap();
    let kinds: Vec<&str> = records.iter().filter_map(|r| r["kind"].as_str()).collect();
    for want in [
        "goal",
        "engine",
        "assistant",
        "tool_result",
        "tool_blocked",
        "goal_finished",
    ] {
        assert!(kinds.contains(&want), "no `{want}` record: {kinds:?}");
    }
    let finished = records
        .iter()
        .find(|r| r["kind"] == "goal_finished")
        .unwrap();
    assert_eq!(finished["engine"], "claude");
    assert_eq!(finished["denials"], 1);
    assert_eq!(finished["tool_calls"], 2);
}

/// The goal reaches the child as one argument, byte for byte, with every
/// character a shell would have acted on intact.
///
/// This is the end-to-end half of the escaping measurement. On Windows it runs
/// through `cmd.exe`, which is where a naive quoting turns `%PATH%` into the
/// machine's real `PATH` — a leak of the user's own filesystem into a prompt.
#[tokio::test]
async fn the_shim_path_hands_the_child_its_goal_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let (cli, argv) = fake_cli(dir.path());
    let h = handoff(dir.path(), cli);
    // Every metacharacter `cmd.exe` acts on except `!`, which the *test's own*
    // delayed-expansion echo cannot print back; `!` is covered by
    // `every_character_cmd_would_act_on_is_escaped_in_the_shim_command_line`.
    let goal = "amp & pipe | pct %PATH% caret ^ paren ( ) lt < gt > tail";
    let _ = run_goal(&h, goal).await;

    let seen = std::fs::read_to_string(&argv)
        .unwrap_or_else(|e| panic!("the fake CLI recorded no argv at {argv:?}: {e}"));
    assert_eq!(
        seen.trim_end_matches(['\r', '\n']),
        goal,
        "the goal was mangled on its way to the child"
    );
    assert!(
        !seen.contains(';') && !seen.to_ascii_lowercase().contains("program files"),
        "an environment variable was expanded into the goal: {seen}"
    );
}

/// A CLI that is not there is a sentence, not a panic and not a silent nothing.
#[tokio::test]
async fn a_missing_cli_ends_the_goal_with_a_sentence_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let h = handoff(
        dir.path(),
        Launch {
            program: dir.path().join("not-installed-here"),
            shim: false,
        },
    );
    let outcome = run_goal(&h, "anything").await;
    match &outcome.ending {
        Ending::Provider(why) => {
            assert!(why.contains("not-installed-here"), "{why}");
            assert!(why.contains("claude engine"), "{why}");
        }
        other => panic!("a missing CLI should end the goal with a provider fault: {other:?}"),
    }
    assert_eq!(outcome.tokens, 0, "nothing ran, so nothing was charged");
}

// endregion: The whole engine, over a fake child
