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

/// The loud failure the compatibility note requires. A hook attached to an event
/// Emma does not implement is a policy the operator believes they have.
#[tokio::test]
async fn a_hook_event_emma_does_not_implement_is_a_startup_error() {
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
