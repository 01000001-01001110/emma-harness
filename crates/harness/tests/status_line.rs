//! The configured status line, against real programs on disk.
//!
//! Three properties, and only the first is about features.
//!
//! **It runs and its stdout is what you get.** That is the whole feature, and a
//! test that only checked the config parsed would prove nothing about it.
//!
//! **A program that will not answer stops being waited for.** The status line is
//! consumed by a repaint. A `run` that can block forever is a terminal the user
//! cannot type into, and no amount of caching upstream fixes a call that never
//! returns — so the bound is asserted here, in wall-clock time, against a
//! program that really does hang.
//!
//! **It cannot be pointed outside `hooks/`, and it never sees the API key.**
//! This is arbitrary code named by a config file, running with Emma's
//! permissions and bypassing the approval gate by design. It is the same trade a
//! hook makes, and it is held to the same boundary — which is worth asserting
//! separately from the hooks that already assert it, because the failure mode is
//! a *second* spawn site that forgot.

mod support;

use emma_harness::{Harness, StatusPayload};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use support::*;

// region: Building a harness with a status line in it
// ---------------------------------------------------------------------------
// Building a harness with a status line in it
//
// The runtime execs the file directly — no shell — so the fixture has to write
// something the platform will actually run, the same way the hook fixtures do.
// ---------------------------------------------------------------------------

/// A status-line program in the platform's directly-executable form.
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
    std::fs::write(&path, body).expect("write status line");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    format!("hooks/{file}")
}

/// A `.claude/` whose `settings.json` names the given program. Claude Code's
/// spelling on purpose: the reason this feature exists is that a file somebody
/// already wrote keeps working.
fn claude_with(tag: &str, name: &str, unix: &str, windows: &str) -> PathBuf {
    let root = scratch(tag).join(".claude");
    let cmd = script(&root.join("hooks"), name, unix, windows);
    write(
        &root.join("settings.json"),
        &format!(r#"{{"statusLine":{{"type":"command","command":"{cmd}"}}}}"#),
    );
    root
}

fn payload() -> StatusPayload {
    StatusPayload {
        cwd: "E:\\emma".into(),
        session_id: "sess-1".into(),
        model: emma_harness::StatusModel {
            id: "claude-opus-4".into(),
            display_name: "claude-opus-4".into(),
        },
        ..Default::default()
    }
}

// endregion: Building a harness with a status line in it

// region: The feature
// ---------------------------------------------------------------------------

/// The whole point, end to end: a `settings.json` written for Claude Code names
/// a program, and what that program prints is what comes back.
#[tokio::test]
async fn what_the_configured_program_prints_is_what_the_status_line_gets() {
    let root = claude_with(
        "status-runs",
        "line",
        "echo 'ctx 12% | main'",
        "echo ctx 12%% ^| main",
    );
    let harness = Harness::load(&root).expect("a status line does not stop the boot");
    let line = harness.status_line().expect("configured");
    let out = line.run(&payload(), 80, 24).await.expect("it ran");
    assert!(out.contains("ctx 12%"), "{out:?}");
    assert!(out.contains("main"), "{out:?}");
    // Nothing was noted, because nothing went wrong.
    assert_eq!(harness.status_line_note(), None);
}

/// The program is told what Claude Code tells it, in Claude Code's own field
/// names — which is the entire reason an existing script is worth honouring
/// rather than a new format being invented.
#[tokio::test]
async fn the_program_reads_the_session_out_of_stdin_the_way_claude_codes_does() {
    // No `jq` on the test machine, so the assertion is on the bytes arriving
    // rather than on a parse of them. What matters is that the JSON reached
    // stdin at all and carries the documented paths.
    let root = claude_with("status-stdin", "line", "cat", "findstr /r \".\"");
    let harness = Harness::load(&root).expect("boot");
    let out = harness
        .status_line()
        .expect("configured")
        .run(&payload(), 80, 24)
        .await
        .expect("it ran");
    let seen: serde_json::Value = serde_json::from_str(out.trim()).expect("stdin was JSON");
    assert_eq!(seen["model"]["display_name"], "claude-opus-4");
    assert_eq!(seen["session_id"], "sess-1");
    assert_eq!(seen["cwd"], "E:\\emma");
}

/// `COLUMNS` is how a status script learns how wide it may draw: Claude Code
/// captures the script's stdout rather than attaching it to the terminal, so
/// nothing inside the script can ask the terminal directly.
#[tokio::test]
async fn the_program_is_told_how_wide_the_terminal_is() {
    let root = claude_with(
        "status-cols",
        "line",
        "echo \"w=$COLUMNS\"",
        "echo w=%COLUMNS%",
    );
    let harness = Harness::load(&root).expect("boot");
    let out = harness
        .status_line()
        .expect("configured")
        .run(&payload(), 97, 24)
        .await
        .expect("ran");
    assert!(
        out.contains("w=97"),
        "COLUMNS did not reach the program: {out:?}"
    );
}

// endregion: The feature

// region: The guarantee — a program that will not answer
// ---------------------------------------------------------------------------
// The guarantee — a program that will not answer
//
// The one property the terminal cannot recover from on its own.
// ---------------------------------------------------------------------------

/// **A status command that hangs must not be waited on.**
///
/// Asserted in wall-clock time against a program that genuinely does not
/// return, because that is the only way to prove it: a mocked "slow" command
/// tests the mock. The bound is the engine's own two seconds, and the assertion
/// allows generous slack over it — what would fail this test is not lateness but
/// *never*, which is the defect it exists to catch.
///
/// Remove the `tokio::time::timeout` inside `StatusLine::run` and this test
/// hangs until the harness kills it. Nothing else in this repository notices.
#[tokio::test]
async fn a_status_command_that_never_returns_is_given_up_on_rather_than_waited_for() {
    let root = claude_with(
        "status-hangs",
        "line",
        // Ten times the engine's bound, and far past any plausible paint
        // interval, so a pass cannot be luck. Not longer, because a killed
        // script's *grandchildren* are not killed with it on either platform —
        // see the note in the assertions below — and the suite pays for that in
        // wall clock.
        "sleep 20",
        "ping -n 20 127.0.0.1 >nul",
    );
    let harness = Harness::load(&root).expect("boot");
    let line = harness.status_line().expect("configured");

    let started = Instant::now();
    let result = line.run(&payload(), 80, 24).await;
    let waited = started.elapsed();

    let err = result.expect_err("a program that never printed must not report success");
    assert!(err.contains("timed out"), "{err}");
    assert!(
        waited < Duration::from_secs(15),
        "the repaint path waited {waited:?} on a hanging status command"
    );
    // …and it really was the bound rather than the program finishing early.
    assert!(waited >= line.timeout(), "{waited:?}");
}

/// A program that fails says why, and says it once. The caller draws the
/// built-in status instead — proving *that* is the terminal's job, not this
/// file's; what is proved here is that the failure is legible rather than a
/// silent blank row.
#[tokio::test]
async fn a_program_that_fails_explains_itself_instead_of_printing_nothing() {
    let root = claude_with(
        "status-fails",
        "line",
        "echo 'no git here' >&2; exit 3",
        "echo no git here 1>&2 & exit /b 3",
    );
    let harness = Harness::load(&root).expect("boot");
    let err = harness
        .status_line()
        .expect("configured")
        .run(&payload(), 80, 24)
        .await
        .expect_err("exit 3 is not a status line");
    assert!(err.contains('3'), "the exit code is missing: {err}");
    assert!(
        err.contains("no git here"),
        "the program's own explanation was thrown away: {err}"
    );
}

// endregion: The guarantee — a program that will not answer

// region: The boundary
// ---------------------------------------------------------------------------

/// The status line is arbitrary code from a config file and it is held to the
/// hook boundary, not to a softer one invented for it.
#[test]
fn a_status_line_pointed_outside_the_hooks_directory_does_not_resolve() {
    let base = scratch("status-escape");
    let outside = base.join("evil.sh");
    std::fs::write(&outside, "#!/bin/sh\ntrue\n").expect("write");
    let root = base.join(".claude");
    // A real `hooks/` exists, so the failure is containment rather than a
    // missing directory — which is the distinction that matters.
    script(&root.join("hooks"), "unused", "true", "exit /b 0");
    write(
        &root.join("settings.json"),
        r#"{"statusLine":{"type":"command","command":"../evil.sh"}}"#,
    );
    let harness = Harness::load(&root).expect("a bad status line must not stop the boot");
    assert!(
        harness.status_line().is_none(),
        "a program outside hooks/ was accepted"
    );
    let note = harness.status_line_note().expect("a reason was owed");
    assert!(note.contains("outside"), "{note}");
    // …and the sentence says what is being drawn instead, so nobody hunts for a
    // status line that is silently the wrong one.
    assert!(note.contains("built-in"), "{note}");
}

/// **The status line never sees `ANTHROPIC_API_KEY`.**
///
/// This process holds the key. A status line is a separate program named by a
/// file, running unattended on a repaint path — it gets what it needs to execute
/// and nothing that would let it spend the user's money. That is the hook
/// allowlist, reused rather than re-decided, and this is the assertion that
/// catches a second spawn site which forgot the `env_clear`.
#[tokio::test]
async fn the_status_program_is_not_handed_the_api_key() {
    // Restored on the panic path too: a failing assertion must not leak a fake
    // key into the rest of the binary.
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
                None => std::env::remove_var("ANTHROPIC_API_KEY"),
            }
        }
    }
    let _restore = Restore(std::env::var_os("ANTHROPIC_API_KEY"));
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-should-never-be-inherited");

    let root = claude_with(
        "status-env",
        "line",
        "echo \"key=[$ANTHROPIC_API_KEY]\"",
        "echo key=[%ANTHROPIC_API_KEY%]",
    );
    let harness = Harness::load(&root).expect("boot");
    let out = harness
        .status_line()
        .expect("configured")
        .run(&payload(), 80, 24)
        .await
        .expect("ran");
    assert!(
        !out.contains("sk-ant-should-never-be-inherited"),
        "the API key reached a program named by a config file: {out:?}"
    );
}

/// Claude Code runs `command` in a shell; Emma execs a contained argv. The
/// inline `jq` its own documentation recommends therefore cannot be honoured —
/// and the refusal has to be a sentence that says what to do, not a boot
/// failure, because the person who copied a working config was not wrong.
#[test]
fn an_inline_shell_status_line_is_noted_and_the_session_still_starts() {
    let root = scratch("status-inline").join(".claude");
    write(
        &root.join("settings.json"),
        r#"{"statusLine":{"type":"command","command":"jq -r .model.display_name"}}"#,
    );
    let harness = Harness::load(&root).expect("an unusable status line is not a boot failure");
    assert!(harness.status_line().is_none());
    let note = harness.status_line_note().expect("a reason was owed");
    assert!(note.contains("shell command"), "{note}");
    assert!(note.contains("hooks/"), "{note}");
}

/// The outer object stays permissive and so does this block. `refreshInterval`
/// and `hideVimModeIndicator` are real Claude Code keys Emma has no opinion
/// about, and a boot failure over one would make this compatibility feature an
/// obstacle rather than a convenience.
#[test]
fn unknown_keys_inside_the_status_line_block_do_not_stop_the_boot() {
    let root = scratch("status-unknown").join(".claude");
    let cmd = script(&root.join("hooks"), "line", "echo hi", "echo hi");
    write(
        &root.join("settings.json"),
        &format!(
            r#"{{"statusLine":{{"type":"command","command":"{cmd}","padding":2,
               "refreshInterval":5,"hideVimModeIndicator":true,"somethingNew":[1,2]}}}}"#
        ),
    );
    let harness = Harness::load(&root).expect("unknown display keys are not typos worth an outage");
    let line = harness.status_line().expect("it still resolved");
    assert_eq!(line.padding, 2, "a key Emma does read was lost");
}

// endregion: The boundary
