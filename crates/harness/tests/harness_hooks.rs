//! Hook execution: the asymmetric failure split, and the environment the child
//! does not get.
//!
//! The split is the design. A `PreToolUse` hook is a **gate**, so anything that
//! is not a clear answer — a crash, a timeout, garbage on stdout — denies: a
//! broken policy check must not degrade into no policy check. A `PostToolUse`
//! hook runs after the side effect happened, so nothing it does or fails to do
//! may change the result; pretending otherwise would make the log lie about what
//! the model saw.

mod support;

use emma_harness::{Harness, HookCall, HookEvent, HookResult};
use std::path::{Path, PathBuf};
use support::*;

// region: Building a harness with one hook in it
// ---------------------------------------------------------------------------
// Building a harness with one hook in it
//
// Every test below is one script, one spine entry, one call. The awkward part
// is that the runtime execs the file directly, so the fixture has to produce
// something the platform will actually run — which is why the scripts are
// written twice and the ordering between file and spine matters.
// ---------------------------------------------------------------------------

/// Writes a hook script in the platform's directly-executable form. Nothing here
/// goes through a shell at call time — the runtime execs the file — so the file
/// itself has to be runnable: `.sh` with a shebang and the executable bit on
/// unix, `.cmd` on Windows.
fn script(dir: &Path, name: &str, unix: &str, windows: &str) -> String {
    std::fs::create_dir_all(dir).expect("mkdir hooks");
    let file = if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        format!("{name}.sh")
    };
    let body = if cfg!(windows) {
        format!("@echo off\r\n{windows}\r\n")
    } else {
        format!("#!/bin/sh\n{unix}\n")
    };
    let path = dir.join(&file);
    std::fs::write(&path, body).expect("write hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    format!("hooks/{file}")
}

/// Build the hook file *before* writing the spine that references it, because a
/// spine entry pointing at a missing script is a load error by design.
fn one_hook(tag: &str, event: &str, name: &str, unix: &str, windows: &str, extra: &str) -> PathBuf {
    let root = scratch(tag).join(".emma");
    let cmd = script(&root.join("hooks"), name, unix, windows);
    write(
        &root.join("config.json"),
        &format!(r#"{{"hooks":{{"h":{{"event":"{event}","command":"{cmd}"{extra}}}}}}}"#),
    );
    root
}

fn call<'a>(args: &'a serde_json::Value) -> HookCall<'a> {
    HookCall {
        tool_name: "Read",
        tool_call_id: "toolu_1",
        args,
        session_id: "sess_1",
        turn_id: "turn_1",
        result: None,
    }
}

/// Puts `ANTHROPIC_API_KEY` back on drop, including on the panic path a failing
/// assertion takes. Restoring at the end of the test body would not survive the
/// failure it exists to be honest about.
struct Restore(Option<std::ffi::OsString>);

impl Drop for Restore {
    fn drop(&mut self) {
        match self.0.take() {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
    }
}

const DENY: (&str, &str) = (
    r#"echo '{"decision":"deny","reason":"after hours"}'"#,
    r#"echo {"decision":"deny","reason":"after hours"}"#,
);

// endregion: Building a harness with one hook in it

// region: PreToolUse: ambiguity resolves to deny
// ---------------------------------------------------------------------------
// PreToolUse: ambiguity resolves to deny
//
// The fail-closed half. Four ways a hook can fail to answer clearly and one way
// it can answer nothing at all — the first four must deny and the fifth must
// allow, and it takes all five to pin the behaviour down.
// ---------------------------------------------------------------------------

/// Arguments reach the child as a real argv, and nothing is joined into a string.
///
/// **Claude Code documents an `args` field — "There is no shell" — and Emma
/// refused a file carrying it as *malformed*.** A `settings.json` that is valid
/// for the program it was written for came back as `unknown field \`args\``,
/// which blames the operator's file for a vocabulary Emma had not learned.
///
/// The shape needs nothing relaxed. A contained path plus arguments *is* an argv
/// exec with no shell — same spawn, same `env_clear`, same containment — so
/// honouring it costs no invariant. The separate and unsettled question is the
/// interpreter form (`node script.js` as one string); this is not that.
///
/// The script echoes its first argument, so a value that arrived joined,
/// re-split, or shell-interpreted would come back different. `a b` is the one
/// that matters: it survives as **one** argument only if nothing ever made it
/// into a string.
#[tokio::test]
async fn hook_arguments_arrive_as_a_real_argv() {
    let root = scratch("hook-args").join(".emma");
    let cmd = script(
        &root.join("hooks"),
        "echoarg",
        r#"printf '{"decision":"deny","reason":"[%s]"}' "$1""#,
        r#"echo {"decision":"deny","reason":"[%~1]"}"#,
    );
    write(
        &root.join("config.json"),
        &format!(
            r#"{{"hooks":{{"h":{{"event":"PreToolUse","command":"{cmd}","args":["a b"]}}}}}}"#
        ),
    );

    let h = Harness::load(&root).expect("a hook with args must load");
    let args = serde_json::json!({});
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;

    assert_eq!(v.runs.len(), 1, "the hook did not run: {:?}", v.runs);
    let reason = v.denied.as_deref().unwrap_or_default();
    assert_eq!(
        reason, "[a b]",
        "the argument did not arrive whole — a value that was joined into a \
         string and re-split would differ here: {reason:?}"
    );
}

/// The base case the other four vary. A hook that says `deny` must produce a
/// verdict the loop cannot mistake for an allow, and it must carry the hook's own
/// reason — a denial the model is told nothing about is one it will retry.
#[tokio::test]
async fn a_hook_that_denies_stops_the_call() {
    let root = one_hook("deny", "PreToolUse", "deny", DENY.0, DENY.1, "");
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({ "path": "x" });
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert_eq!(v.denied.as_deref(), Some("after hours"));
    assert!(v.is_denied());
    assert_eq!(v.runs.len(), 1);
}

/// Delete this and a hook with a syntax error stops guarding anything while
/// still appearing in `config check` — the failure mode where the operator has a
/// policy on paper and none in force.
#[tokio::test]
async fn a_hook_that_exits_non_zero_denies() {
    let root = one_hook("nonzero", "PreToolUse", "x", "exit 3", "exit /b 3", "");
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert!(
        v.denied.is_some(),
        "a crashed gate must not degrade into no gate"
    );
    assert_eq!(v.runs[0].exit_code, Some(3));
}

/// A hang is the failure most likely to be reasoned away as "it will finish".
/// Both halves are asserted: it denies, and it denies *soon*. A gate that
/// eventually denies after blocking the turn is a different bug, not a pass.
#[tokio::test]
async fn a_hook_that_times_out_denies() {
    let root = one_hook(
        "timeout",
        "PreToolUse",
        "slow",
        "sleep 5",
        "ping -n 6 127.0.0.1 >nul",
        r#","timeout_ms":300"#,
    );
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let started = std::time::Instant::now();
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert!(
        v.denied.is_some(),
        "a hook that never answers must not allow"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "the timeout must be enforced well inside anyone's patience"
    );
    assert!(v.runs[0].stderr.contains("timed out"), "{:?}", v.runs[0]);
}

/// The subtle one, and the reason the parse failure is not tolerated: a hook
/// that exits 0 and prints something unreadable has answered, just not in a
/// language the runtime speaks. Treating that as silence would make a
/// half-written deny read as an allow.
#[tokio::test]
async fn garbage_on_stdout_denies() {
    let root = one_hook(
        "garbage",
        "PreToolUse",
        "garbage",
        "echo not-json-at-all",
        "echo not-json-at-all",
        "",
    );
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert!(v.denied.is_some(), "an unreadable answer is not an answer");
}

/// Empty output is the normal answer for an observer, and it means allow.
///
/// This is the counterweight to the four tests above: without it they are all
/// satisfied by a runtime that denies unconditionally, which would pass the
/// suite and make every configured hook a wall. The second assertion is the
/// other half — allowing is not the same as not running, and the run still has
/// to appear in the log.
#[tokio::test]
async fn silence_is_allow() {
    let root = one_hook("silent", "PreToolUse", "quiet", "exit 0", "exit /b 0", "");
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert!(v.denied.is_none());
    assert_eq!(v.runs.len(), 1, "it still ran and is still recorded");
}

// endregion: PreToolUse: ambiguity resolves to deny

// region: PostToolUse: the result stands
// ---------------------------------------------------------------------------
// PostToolUse: the result stands
//
// The fail-open half, and the same three failures reaching the opposite ruling
// because the side effect has already happened. A hook here may annotate and
// may be recorded; it may not change what the model was told.
// ---------------------------------------------------------------------------

/// The other half of the asymmetry, and the one that reads as a missing safety
/// check until you notice the ordering: by the time a `PostToolUse` hook runs,
/// the file is written and the command has run. There is nothing left to
/// prevent, so a failure here can only be recorded — which is what the second
/// and third assertions are for. A runtime that let this deny would abort turns
/// over side effects that had already happened.
#[tokio::test]
async fn a_post_tool_use_failure_leaves_the_result_standing() {
    let root = one_hook(
        "post-fail",
        "PostToolUse",
        "boom",
        "exit 9",
        "exit /b 9",
        "",
    );
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let mut c = call(&args);
    c.result = Some(HookResult {
        content: "the file contents",
        truncated: false,
        error: None,
    });
    let v = h.run_hooks(HookEvent::PostToolUse, &c).await;
    assert!(
        v.denied.is_none(),
        "the tool already ran and its result is already recorded — there is \
         nothing left to deny, and saying otherwise makes the log lie"
    );
    assert_eq!(v.runs.len(), 1, "the failure is still recorded");
    assert_eq!(v.runs[0].exit_code, Some(9));
}

/// A `deny` from a `PostToolUse` hook is recorded and ignored, for the same
/// reason.
#[tokio::test]
async fn a_post_tool_use_deny_is_recorded_and_ignored() {
    let root = one_hook(
        "post-deny",
        "PostToolUse",
        "late",
        r#"echo '{"decision":"deny","reason":"too late"}'"#,
        r#"echo {"decision":"deny","reason":"too late"}"#,
        "",
    );
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let v = h.run_hooks(HookEvent::PostToolUse, &call(&args)).await;
    assert!(v.denied.is_none());
    assert_eq!(v.runs[0].reason.as_deref(), Some("too late"));
}

/// A hook can annotate a result; it can never rewrite one.
#[tokio::test]
async fn context_is_additive() {
    let root = one_hook(
        "ctx",
        "PostToolUse",
        "note",
        r#"echo '{"context":"reviewed"}'"#,
        r#"echo {"context":"reviewed"}"#,
        "",
    );
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let v = h.run_hooks(HookEvent::PostToolUse, &call(&args)).await;
    assert_eq!(v.context, vec!["reviewed".to_string()]);
    assert!(v.denied.is_none());
}

// endregion: PostToolUse: the result stands

// region: UserPromptSubmit: enrichment fails open, and says so
// ---------------------------------------------------------------------------
// UserPromptSubmit: enrichment fails open, and says so
//
// The third ruling. This event exists to *add* to a turn rather than to gate
// one, so the four failures that deny at `PreToolUse` must not deny here — a
// hook script with a syntax error would otherwise refuse every prompt the user
// types. What replaces the denial is a notice: the enrichment is lost visibly.
//
// Blocking is still real, because a hook that says `block` has answered rather
// than failed, and both of Claude Code's spellings of it are honoured.
// ---------------------------------------------------------------------------

/// The whole point of the event, in Claude Code's spelling — which is the one
/// spelling that matters, because it is what the hooks people already have print.
/// Before this event existed that JSON was an unknown field, i.e. unparseable
/// stdout.
#[tokio::test]
async fn context_reaches_the_caller_in_claude_codes_spelling() {
    const OUT: &str = r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"branch: main"}}"#;
    let root = one_hook(
        "ups-ctx",
        "UserPromptSubmit",
        "ctx",
        &format!("echo '{OUT}'"),
        &format!("echo {OUT}"),
        "",
    );
    let h = Harness::load(&root).expect("load");
    let v = h
        .on_user_prompt("port the middleware", "sess_1", "s.jsonl")
        .await;
    assert_eq!(v.context, vec!["branch: main".to_string()]);
    assert!(!v.is_blocked());
    assert!(v.notices().is_empty(), "{:?}", v.notices());
}

/// Emma's own spelling keeps working, so a hook written against `PostToolUse`
/// here does not have to be rewritten to enrich a prompt.
#[tokio::test]
async fn context_reaches_the_caller_in_emmas_spelling() {
    let root = one_hook(
        "ups-ctx2",
        "UserPromptSubmit",
        "ctx",
        r#"echo '{"context":"branch: main"}'"#,
        r#"echo {"context":"branch: main"}"#,
        "",
    );
    let h = Harness::load(&root).expect("load");
    let v = h.on_user_prompt("hi", "sess_1", "s.jsonl").await;
    assert_eq!(v.context, vec!["branch: main".to_string()]);
}

/// A hook is told the words a person typed, under the names the scripts that
/// exist already read. Proved from the child's side: the payload is whatever
/// arrived on its stdin, not whatever the builder meant to send.
#[tokio::test]
async fn the_hook_is_told_the_prompt_under_claude_codes_field_names() {
    let root = scratch("ups-payload").join(".emma");
    let dump = root.join("stdin.json");
    let cmd = script(
        &root.join("hooks"),
        "dump",
        &format!("cat > '{}'", dump.display()),
        &format!("findstr \"^\" > \"{}\"", dump.display()),
    );
    write(
        &root.join("config.json"),
        &format!(r#"{{"hooks":{{"h":{{"event":"UserPromptSubmit","command":"{cmd}"}}}}}}"#),
    );
    let h = Harness::load(&root).expect("load");
    let _ = h
        .on_user_prompt("port the middleware", "sess_7", "E:\\logs\\sess_7.jsonl")
        .await;

    let seen = std::fs::read_to_string(&dump).expect("the hook must have run");
    let seen: serde_json::Value = serde_json::from_str(seen.trim()).expect("json on stdin");
    assert_eq!(seen["prompt"], "port the middleware");
    assert_eq!(seen["session_id"], "sess_7");
    assert_eq!(seen["hook_event_name"], "UserPromptSubmit");
    assert_eq!(seen["transcript_path"], "E:\\logs\\sess_7.jsonl");
    assert!(
        !seen["cwd"].as_str().unwrap_or_default().is_empty(),
        "a hook that cannot tell where it is cannot report a branch: {seen}"
    );
    // Emma's own key is there too, so one hook can read the event the same way
    // on all three dispatch sites.
    assert_eq!(seen["event"], "UserPromptSubmit");
    // Not sent, because Emma's approval gate is not Claude Code's mode enum and
    // a plausible answer is one a hook would branch on and be wrong about.
    assert!(seen.get("permission_mode").is_none(), "{seen}");
}

/// The failure this whole ruling is about. A hook that hangs must not hang the
/// session, and — the half a fail-open design gets wrong — the turn must not
/// quietly proceed as though nothing was configured.
#[tokio::test]
async fn a_prompt_hook_that_hangs_loses_its_context_loudly_and_never_blocks() {
    let root = one_hook(
        "ups-slow",
        "UserPromptSubmit",
        "slow",
        "sleep 5",
        "ping -n 6 127.0.0.1 >nul",
        r#","timeout_ms":300"#,
    );
    let h = Harness::load(&root).expect("load");
    let started = std::time::Instant::now();
    let v = h.on_user_prompt("hi", "sess_1", "s.jsonl").await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "the prompt waited on the hook: {:?}",
        started.elapsed()
    );
    assert!(
        !v.is_blocked(),
        "a broken enricher must not lock the user out of their own agent"
    );
    assert!(
        v.context.is_empty(),
        "a timed-out hook's output is discarded"
    );
    let notices = v.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("timed out"), "{notices:?}");
    assert!(
        notices[0].contains("h"),
        "the notice must name the hook: {notices:?}"
    );
}

/// A crash and unreadable output are the other two ways to fail, and they land
/// the same way — with one exception, which is the next test.
#[tokio::test]
async fn a_crash_loses_the_context_and_lets_the_prompt_through() {
    let root = one_hook(
        "ups-crash",
        "UserPromptSubmit",
        "x",
        "exit 3",
        "exit /b 3",
        "",
    );
    let h = Harness::load(&root).expect("load");
    let v = h.on_user_prompt("hi", "sess_1", "s.jsonl").await;
    assert!(!v.is_blocked(), "a crashing hook blocked the prompt");
    assert!(v.context.is_empty());
    assert_eq!(v.notices().len(), 1, "{:?}", v.notices());
}

/// Plain stdout **is** the context on this event, and that is the shape people
/// actually write: a script that echoes a line and exits zero.
///
/// This case used to sit in the test above, asserting that non-JSON stdout was
/// lost — and the feature was shipped that way. It survived every test and
/// failed the first time a real hook ran: `echo`, exit 0, and Emma recorded
/// `unparseable stdout` and threw the line away. The JSON object is the
/// elaborate form, not the required one. The other events keep the old
/// resolution, because free text means nothing to them and an unreadable answer
/// there is a hook that failed to say what it meant.
#[tokio::test]
async fn plain_stdout_is_the_context_rather_than_a_parse_failure() {
    let root = one_hook(
        "ups-plain",
        "UserPromptSubmit",
        "x",
        "echo the branch is main",
        "echo the branch is main",
        "",
    );
    let h = Harness::load(&root).expect("load");
    let v = h.on_user_prompt("hi", "sess_1", "s.jsonl").await;
    assert!(!v.is_blocked());
    assert_eq!(v.context, vec!["the branch is main".to_string()]);
    assert!(
        v.notices().is_empty(),
        "a hook that worked reported a problem: {:?}",
        v.notices()
    );
}

/// Exit 2 is the one non-zero exit that means "no" rather than "broken", and the
/// message the user reads is the hook's stderr. Without this arm, an operator's
/// `exit 2` guard is a crash notice and the prompt they meant to stop goes
/// through.
#[tokio::test]
async fn exit_two_blocks_the_prompt_with_stderr_as_the_reason() {
    let root = one_hook(
        "ups-exit2",
        "UserPromptSubmit",
        "gate",
        "echo 'no prompts on the release branch' >&2; exit 2",
        "echo no prompts on the release branch 1>&2\r\nexit /b 2",
        "",
    );
    let h = Harness::load(&root).expect("load");
    let v = h.on_user_prompt("ship it", "sess_1", "s.jsonl").await;
    assert_eq!(
        v.blocked.as_deref(),
        Some("no prompts on the release branch")
    );
}

/// The JSON spelling of the same answer, and the one Claude Code documents
/// first. `deny` is accepted as well, because that is Emma's word everywhere
/// else and an operator should not have to know which of their hooks is which.
#[tokio::test]
async fn a_decision_of_block_stops_the_prompt() {
    for (tag, word) in [("ups-block", "block"), ("ups-deny", "deny")] {
        let json = format!(r#"{{"decision":"{word}","reason":"not during the freeze"}}"#);
        let root = one_hook(
            tag,
            "UserPromptSubmit",
            "gate",
            &format!("echo '{json}'"),
            &format!("echo {json}"),
            "",
        );
        let h = Harness::load(&root).expect("load");
        let v = h.on_user_prompt("ship it", "sess_1", "s.jsonl").await;
        assert_eq!(v.blocked.as_deref(), Some("not during the freeze"), "{tag}");
    }
}

/// Context is what the model reads and `systemMessage` is what the user reads.
/// A runtime that mixed them would either bill the user for a warning meant for
/// them, or hide from them a sentence a hook wrote for them.
#[tokio::test]
async fn a_system_message_is_shown_to_the_user_and_not_to_the_model() {
    const OUT: &str = r#"{"systemMessage":"the branch is stale","hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"branch: main"}}"#;
    let root = one_hook(
        "ups-msg",
        "UserPromptSubmit",
        "msg",
        &format!("echo '{OUT}'"),
        &format!("echo {OUT}"),
        "",
    );
    let h = Harness::load(&root).expect("load");
    let v = h.on_user_prompt("hi", "sess_1", "s.jsonl").await;
    assert_eq!(v.context, vec!["branch: main".to_string()]);
    assert_eq!(v.notices(), vec!["the branch is stale".to_string()]);
}

/// The user pays for injected context by the token, on this turn and on every
/// later turn that replays it, so a program that will not stop printing is cut —
/// and the cut is named in the text, because a model shown half a sentence
/// reasons about the half it was given.
#[tokio::test]
async fn a_hook_cannot_fill_the_prompt_and_a_cut_says_so() {
    // 20,000 characters of context, twice the cap. The script prints a file
    // rather than building the string inline, because the two shells disagree
    // about everything except `cat`/`type`.
    let base = scratch("ups-flood");
    let flood = base.join("flood.json");
    write(
        &flood,
        &format!(r#"{{"context":"{}"}}"#, "x".repeat(20_000)),
    );
    let root = base.join(".emma");
    let cmd = script(
        &root.join("hooks"),
        "flood",
        &format!("cat '{}'", flood.display()),
        &format!("type \"{}\"", flood.display()),
    );
    write(
        &root.join("config.json"),
        &format!(r#"{{"hooks":{{"h":{{"event":"UserPromptSubmit","command":"{cmd}"}}}}}}"#),
    );
    let h = Harness::load(&root).expect("load");
    let v = h.on_user_prompt("hi", "sess_1", "s.jsonl").await;
    let ctx = v.context.first().expect("some context survived");
    assert!(
        ctx.len() < 11_000,
        "a hook put {} characters in front of the model",
        ctx.len()
    );
    assert!(
        ctx.contains("was cut"),
        "the loss was not named: {}",
        &ctx[ctx.len().saturating_sub(200)..]
    );
}

/// A reason is arbitrary text from an arbitrary program, and the cap used to be
/// a `String::truncate` — which panics on a byte index inside a character. A
/// 400-byte cut through a `→` took the process down at the exact moment a policy
/// was being explained.
#[tokio::test]
async fn a_multibyte_reason_longer_than_the_cap_does_not_panic() {
    let long: String = "→".repeat(300);
    let json = format!(r#"{{"decision":"block","reason":"{long}"}}"#);
    let root = one_hook(
        "ups-utf8",
        "UserPromptSubmit",
        "gate",
        &format!("printf '%s' '{json}'"),
        &format!("echo {json}"),
        "",
    );
    let h = Harness::load(&root).expect("load");
    let v = h.on_user_prompt("hi", "sess_1", "s.jsonl").await;
    let reason = v.blocked.expect("blocked");
    assert!(reason.starts_with('→'), "{reason}");
    assert!(
        reason.len() <= 400,
        "the cap did not apply: {}",
        reason.len()
    );
}

/// Nothing configured is not a special case anywhere: no runs, no context, no
/// block, and a caller that needs no `if` around the call.
#[tokio::test]
async fn a_harness_with_no_prompt_hooks_answers_nothing() {
    let root = scratch("ups-none").join(".emma");
    write(&root.join("config.json"), "{}");
    let h = Harness::load(&root).expect("load");
    let v = h.on_user_prompt("hi", "sess_1", "s.jsonl").await;
    assert!(v.runs.is_empty() && v.context.is_empty() && !v.is_blocked());
}

/// A `UserPromptSubmit` hook has no tool name to match, so a matcher on one is a
/// filter that can never be true — the same failure as a hook attached to an
/// event that never fires, and it gets the same loud answer.
#[tokio::test]
async fn a_matcher_on_a_prompt_hook_is_a_startup_error() {
    let root = one_hook(
        "ups-matcher",
        "UserPromptSubmit",
        "ctx",
        "exit 0",
        "exit /b 0",
        r#","matcher":"Bash""#,
    );
    let err = Harness::load(&root).expect_err("a matcher that can never match must not load");
    let msg = format!("{err:#}");
    assert!(msg.contains("Bash"), "{msg}");
    assert!(msg.contains("no tool name"), "{msg}");

    // …and the empty one, which is what a group written for a tool event looks
    // like when it was copied for this one, asks for nothing and loads.
    let root = one_hook(
        "ups-matcher-empty",
        "UserPromptSubmit",
        "ctx",
        r#"echo '{"context":"ok"}'"#,
        r#"echo {"context":"ok"}"#,
        r#","matcher":"""#,
    );
    let h = Harness::load(&root).expect("an empty matcher is not a matcher");
    let v = h.on_user_prompt("hi", "sess_1", "s.jsonl").await;
    assert_eq!(v.context, vec!["ok".to_string()], "the hook never ran");
}

/// The compatibility claim, tested against Claude Code's own file format rather
/// than Emma's: a `.claude/settings.json` that already has a `UserPromptSubmit`
/// entry in it loads and fires, unchanged.
#[tokio::test]
async fn a_claude_settings_file_with_a_prompt_hook_works_unchanged() {
    let root = scratch("ups-claude").join(".claude");
    let cmd = script(
        &root.join("hooks"),
        "nudge",
        r#"echo '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"[task-tracking] keep a task list"}}'"#,
        r#"echo {"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"[task-tracking] keep a task list"}}"#,
    );
    write(
        &root.join("settings.json"),
        &format!(
            r#"{{"model":"opus","hooks":{{"UserPromptSubmit":[{{"hooks":[
               {{"type":"command","command":"$CLAUDE_PROJECT_DIR/.claude/{cmd}"}}]}}]}}}}"#
        ),
    );
    write(&root.join("CLAUDE.md"), "rules");
    let h = Harness::load(&root).expect("load");
    let v = h.on_user_prompt("hi", "sess_1", "s.jsonl").await;
    assert_eq!(
        v.context,
        vec!["[task-tracking] keep a task list".to_string()]
    );
}

/// Widening the reply shape to accept Claude Code's JSON must not turn a `deny`
/// into an allow. Its `PreToolUse` spelling used to be unparseable — which
/// denied, for the wrong reason but with the right result — so this pins the
/// right reason: the verdict is read, not fallen back to.
#[tokio::test]
async fn claude_codes_pre_tool_use_spelling_of_deny_is_honoured() {
    const OUT: &str = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"not that path"}}"#;
    let root = one_hook(
        "cc-deny",
        "PreToolUse",
        "gate",
        &format!("echo '{OUT}'"),
        &format!("echo {OUT}"),
        "",
    );
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert_eq!(v.denied.as_deref(), Some("not that path"));
    assert_eq!(
        v.runs[0].exit_code,
        Some(0),
        "it must be the decision that denied, not a crash"
    );
}

// endregion: UserPromptSubmit: enrichment fails open, and says so

// region: The environment the child does not get
// ---------------------------------------------------------------------------
// The environment the child does not get
//
// `env_clear` down to the allowlist, proved from the child's side rather than
// by reading the constant. A test that checked the allowlist would pass even if
// `env_clear` were never called.
// ---------------------------------------------------------------------------

/// This process holds the provider key. A hook is operator-authored, but it is
/// still a separate program at the highest-privilege point of the turn: it must
/// not be able to call a model or a paid API as us.
#[tokio::test]
async fn the_child_environment_carries_no_api_key() {
    // The variable has to be set on *this* process — the whole point is that the
    // child does not inherit what the parent holds — so this is the one test in
    // the crate that cannot avoid touching process-wide state. It can avoid
    // leaving it there: without the restore below, every test that ran afterwards
    // in this binary inherited a fake key, and any of them that grew a dependency
    // on the real one would have failed for a reason nobody could find.
    let previous = std::env::var_os("ANTHROPIC_API_KEY");
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-should-not-be-inherited");
    let _restore = Restore(previous);

    let root = scratch("env").join(".emma");
    let dump = root.join("env.txt");
    let cmd = script(
        &root.join("hooks"),
        "dumpenv",
        &format!("env > '{}'", dump.display()),
        &format!("set > \"{}\"", dump.display()),
    );
    write(
        &root.join("config.json"),
        &format!(r#"{{"hooks":{{"h":{{"event":"PreToolUse","command":"{cmd}"}}}}}}"#),
    );

    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let _ = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;

    let seen = std::fs::read_to_string(&dump).expect("the hook must have run");
    assert!(
        !seen.contains("should-not-be-inherited"),
        "an API key reached the hook process:\n{seen}"
    );
    assert!(
        !seen.to_uppercase().contains("ANTHROPIC_API_KEY"),
        "the variable name is present even if the value looks scrubbed:\n{seen}"
    );
}

// endregion: The environment the child does not get

// region: Load-time containment
// ---------------------------------------------------------------------------
// Load-time containment
//
// Everything that must be settled before a hook exists at all: where it may
// live, what it may match, how long it may take, which events it may attach to,
// and whether a persona may switch it off. All of these fail the boot, because
// discovering them at the first tool call is discovering them too late.
// ---------------------------------------------------------------------------

/// Containment, tested at the load rather than at the call. Without it, the
/// spine is a way to run any executable on the box with Emma's permissions —
/// and Emma's permissions include writing the user's source tree. `..` is the
/// cheap escape, which is why `resolve` canonicalises before comparing rather
/// than pattern-matching the string.
#[tokio::test]
async fn a_hook_command_outside_the_hooks_directory_fails_the_load() {
    let base = scratch("escape");
    let root = base.join(".emma");
    std::fs::create_dir_all(root.join("hooks")).expect("mkdir");
    let outside = base.join("evil.sh");
    std::fs::write(&outside, "#!/bin/sh\ntrue\n").expect("write");
    write(
        &root.join("config.json"),
        r#"{"hooks":{"h":{"event":"PreToolUse","command":"../evil.sh"}}}"#,
    );

    let err = Harness::load(&root).expect_err("a path escaping hooks/ is a load error");
    assert!(format!("{err:#}").contains("evil.sh"), "{err:#}");
}

/// A typo'd path must not become a hook that quietly never fires. It fails at
/// boot, where someone is watching, rather than at the first `Bash` call — and
/// because `PreToolUse` is fail-closed, deferring it would turn a typo into a
/// tool that denies everything for reasons nobody can see.
#[tokio::test]
async fn a_hook_command_that_does_not_exist_fails_the_load() {
    let root = scratch("missing").join(".emma");
    std::fs::create_dir_all(root.join("hooks")).expect("mkdir");
    write(
        &root.join("config.json"),
        r#"{"hooks":{"h":{"event":"PreToolUse","command":"hooks/nope.sh"}}}"#,
    );
    let err = Harness::load(&root).expect_err("a spine entry naming a missing script must fail");
    assert!(format!("{err:#}").contains("nope.sh"), "{err:#}");
}

/// A matcher is matched in full, so `Read` cannot silently guard `ReadFile` —
/// the near miss that looks like a working policy.
#[tokio::test]
async fn matchers_are_anchored() {
    let root = one_hook(
        "matcher",
        "PreToolUse",
        "deny",
        DENY.0,
        DENY.1,
        r#","matcher":"Rea""#,
    );
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert!(
        v.denied.is_none() && v.runs.is_empty(),
        "`Rea` must not match `Read`"
    );

    // And the positive half, so the test cannot pass by never matching anything.
    let root = one_hook(
        "matcher-hit",
        "PreToolUse",
        "deny",
        DENY.0,
        DENY.1,
        r#","matcher":"Read""#,
    );
    let h = Harness::load(&root).expect("load");
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert!(v.denied.is_some(), "`Read` must match `Read`");
}

/// A hook is a gate, not a job runner, so the engine caps what config may ask
/// for regardless of what it asks for.
#[tokio::test]
async fn the_engine_caps_the_timeout_config_asks_for() {
    let root = one_hook(
        "clamp",
        "PreToolUse",
        "slow",
        "sleep 30",
        "ping -n 31 127.0.0.1 >nul",
        r#","timeout_ms":600000"#,
    );
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let started = std::time::Instant::now();
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert!(v.denied.is_some());
    // Both ends, because each one alone is nearly free to pass. An upper bound of
    // twenty seconds against a ten-second cap is satisfied by a cap that regressed
    // to nineteen; and an upper bound alone is satisfied by a hook that never
    // spawned at all, which denies instantly. The window is the cap.
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(13),
        "config asked for ten minutes and the engine must have refused it: {elapsed:?}"
    );
    assert!(
        elapsed > std::time::Duration::from_secs(8),
        "this returned before the cap, so it is not the cap being measured — a \
         hook that failed to spawn denies in a millisecond: {elapsed:?}"
    );
}

/// The `EMMA_CLAUDE_HOOKS` variable, taken away for the duration and put back.
///
/// **`DEF-033`'s fix reached one test binary and this is the other.** A reviewer
/// exported the variable and ran the suite:
///
/// ```text
/// $ EMMA_CLAUDE_HOOKS=skip-unknown cargo test -p emma-harness
/// test a_hook_event_emma_does_not_implement_is_a_startup_error ... FAILED
/// ```
///
/// The suite still depended on the shell it was launched from, which the other
/// binary's own comment calls not-evidence. Each integration test is its own
/// process, so a guard living in `claude_compat.rs` cannot serialise anything
/// here — this is a second copy on purpose, not drift.
static HOOK_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct StrictHooks {
    _guard: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl StrictHooks {
    fn take() -> Self {
        let guard = HOOK_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("EMMA_CLAUDE_HOOKS");
        std::env::remove_var("EMMA_CLAUDE_HOOKS");
        Self {
            _guard: guard,
            previous,
        }
    }

    /// The same lock, with the opt-in turned on rather than off.
    fn skipping() -> Self {
        let me = Self::take();
        std::env::set_var("EMMA_CLAUDE_HOOKS", "skip-unknown");
        me
    }
}

impl Drop for StrictHooks {
    fn drop(&mut self) {
        // Restored on the panicking path too: the tests that use it are the
        // ones most likely to fail.
        match self.previous.take() {
            Some(v) => std::env::set_var("EMMA_CLAUDE_HOOKS", v),
            None => std::env::remove_var("EMMA_CLAUDE_HOOKS"),
        }
    }
}

/// The loud failure the compatibility note requires. A hook attached to an event
/// Emma does not implement is a policy the operator believes they have.
#[tokio::test]
async fn a_hook_event_emma_does_not_implement_is_a_startup_error() {
    // Without this the test asserts a property of the shell it was launched
    // from. See `StrictHooks`.
    let _strict = StrictHooks::take();
    let root = scratch("unknown-event").join(".emma");
    std::fs::create_dir_all(root.join("hooks")).expect("mkdir");
    let cmd = script(&root.join("hooks"), "x", "exit 0", "exit /b 0");
    write(
        &root.join("config.json"),
        &format!(r#"{{"hooks":{{"h":{{"event":"SessionStart","command":"{cmd}"}}}}}}"#),
    );
    let err = Harness::load(&root).expect_err("an unimplemented event must not be skipped");
    let msg = format!("{err:#}");
    assert!(msg.contains("SessionStart"), "{msg}");
    assert!(
        msg.contains("PreToolUse"),
        "the error must say what is implemented: {msg}"
    );
}

/// A hook the persona did not enable is never resolved, so it costs nothing —
/// including `"hooks": []`, which turns all of them off.
#[tokio::test]
async fn a_persona_can_turn_every_hook_off() {
    let root = scratch("disabled").join(".emma");
    let cmd = script(&root.join("hooks"), "deny", DENY.0, DENY.1);
    write(
        &root.join("config.json"),
        &format!(
            r#"{{"default_persona":"a","personas":{{"a":{{"hooks":[]}}}},
                 "hooks":{{"h":{{"event":"PreToolUse","command":"{cmd}"}}}}}}"#
        ),
    );
    write(&root.join("personas/a/rules.md"), "rules");
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert!(v.runs.is_empty());
}

/// The inverse of the test above, and the reason `resolve` checks the enabled
/// list against the declarations before it resolves anything: a persona naming a
/// hook the spine does not define is asking for a guard that does not exist, and
/// silently enabling nothing looks identical to enabling it.
#[tokio::test]
async fn a_persona_enabling_an_undeclared_hook_fails_the_load() {
    let root = scratch("undeclared").join(".emma");
    write(
        &root.join("config.json"),
        r#"{"default_persona":"a","personas":{"a":{"hooks":["ghost"]}}}"#,
    );
    write(&root.join("personas/a/rules.md"), "rules");
    let err = Harness::load(&root).expect_err("enabling an undeclared hook must fail");
    assert!(format!("{err:#}").contains("ghost"), "{err:#}");
}

// endregion: Load-time containment

/// The opt-in names the hook it skipped, and does not hand over a sentinel.
///
/// **An internal token reached the operator as the entire diagnosis.**
/// `HookEvent::parse` signalled "the operator opted into skipping this" by
/// returning an error whose *message* was `EMMA_SKIP_HOOK:<event>`, and only
/// the `.claude` caller knew to strip it. The `.emma` caller used `?`, so a
/// hook here produced:
///
/// ```text
/// Error: …\.emma\config.json: hook `h`
/// Caused by:
///     EMMA_SKIP_HOOK:SessionStart
/// ```
///
/// Certified on the real binary before the fix, and reachable by following
/// Emma's own advice: the strict message tells the operator to set the
/// variable, and setting it replaced a sentence naming the implemented events
/// and the remedy with a token.
///
/// The sentinel is gone rather than handled in a second place — the answer is
/// typed now, so there is nothing left for a third caller to forget.
#[tokio::test]
async fn the_skip_opt_in_names_the_hook_rather_than_leaking_a_sentinel() {
    let _strict = StrictHooks::skipping();
    let root = scratch("skip-sentinel").join(".emma");
    std::fs::create_dir_all(root.join("hooks")).expect("mkdir");
    let cmd = script(&root.join("hooks"), "x", "exit 0", "exit /b 0");
    write(
        &root.join("config.json"),
        &format!(r#"{{"hooks":{{"h":{{"event":"SessionStart","command":"{cmd}"}}}}}}"#),
    );

    let ok = script(&root.join("hooks"), "ok", "exit 0", "exit /b 0");
    write(
        &root.join("config.json"),
        &format!(
            r#"{{"hooks":{{"bad":{{"event":"SessionStart","command":"{cmd}"}},"good":{{"event":"PreToolUse","command":"{ok}"}}}}}}"#
        ),
    );

    // Before the fix this returned Err carrying `EMMA_SKIP_HOOK:SessionStart`
    // as the whole message.
    let h = Harness::load(&root).expect("the opt-in must let the run start");

    // And it dropped only the unknown one. Asserting the load succeeded would
    // pass just as well if the skip had thrown every hook away, which is the
    // failure worth guarding against: a run that starts and silently enforces
    // nothing is the shape this whole area exists to prevent.
    let v = h
        .run_hooks(HookEvent::PreToolUse, &call(&serde_json::json!({})))
        .await;
    assert_eq!(
        v.runs.len(),
        1,
        "the implemented hook was dropped along with the unimplemented one: {:?}",
        v.runs
    );
}
