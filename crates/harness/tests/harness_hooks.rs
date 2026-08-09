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

const DENY: (&str, &str) = (
    r#"echo '{"decision":"deny","reason":"after hours"}'"#,
    r#"echo {"decision":"deny","reason":"after hours"}"#,
);

// ---------------------------------------------------------------------------
// PreToolUse: ambiguity resolves to deny
// ---------------------------------------------------------------------------

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
#[tokio::test]
async fn silence_is_allow() {
    let root = one_hook("silent", "PreToolUse", "quiet", "exit 0", "exit /b 0", "");
    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({});
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert!(v.denied.is_none());
    assert_eq!(v.runs.len(), 1, "it still ran and is still recorded");
}

// ---------------------------------------------------------------------------
// PostToolUse: the result stands
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_post_tool_use_failure_leaves_the_result_standing() {
    let root = one_hook("post-fail", "PostToolUse", "boom", "exit 9", "exit /b 9", "");
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

// ---------------------------------------------------------------------------
// The environment the child does not get
// ---------------------------------------------------------------------------

/// This process holds the provider key. A hook is operator-authored, but it is
/// still a separate program at the highest-privilege point of the turn: it must
/// not be able to call a model or a paid API as us.
#[tokio::test]
async fn the_child_environment_carries_no_api_key() {
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-should-not-be-inherited");

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

// ---------------------------------------------------------------------------
// Load-time containment
// ---------------------------------------------------------------------------

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
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "config asked for ten minutes and the engine must have refused it: {:?}",
        started.elapsed()
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
