//! What Emma claims about a hook's own exit, and what it claims about the
//! processes that hook started.
//!
//! **The defect these fixtures were built for.** A hook that spawns something
//! and exits 0 in milliseconds was reported as *timed out after 5000ms* and its
//! output thrown away. Nothing about the hook was slow: the thing it started
//! inherited the stdout pipe and held the write end open, and `exec` was reading
//! for an end-of-file that only arrives when the last writer closes. The bug was
//! never "a grandchild is alive" — it was keying the hook's completion on pipe
//! EOF instead of on the hook's own exit.
//!
//! **The ruling these fixtures pin.** The owner ruled on 2026-08-14 that a hook
//! which spawns something meant to outlive it is a *supported use*, not a leak
//! (`notes/plans/process-lifetime.md`, §1). So the surviving grandchild is not a
//! hazard to be tolerated here — it is a guarantee with a test on it, and
//! `a_daemonizing_hooks_child_is_still_running_after_emma_walks_away` fails if
//! anybody ever adds a job object, a process group or a tree kill.
//!
//! **Why every liveness claim goes through a process handle.** `taskkill /T /F`
//! returns when termination has been *requested*, and `tasklist` is a formatted
//! string a pid can be recycled out from under. Only
//! `OpenProcess(SYNCHRONIZE)` + `WaitForSingleObject` answers "is this process
//! gone" about the process we meant. That was paid for once already —
//! `notes/lessons/a-flaky-test-was-the-only-symptom-of-a-real-leak.md`.
//!
//! **Windows only, and said plainly rather than hidden in a `cfg`.** This is the
//! platform the defect was found on and the only one this box can certify. The
//! unix arm of the fix is reasoned in the plan and compile-checked here; §6 of
//! the plan names the Linux run that would settle it, including the one place
//! unix genuinely differs (a daemon that keeps writing gets SIGPIPE when Emma
//! drops the read end, and Windows has no such signal).

mod support;

#[cfg(windows)]
use emma_harness::Harness;
#[cfg(windows)]
use std::path::Path;
#[cfg(windows)]
use std::time::{Duration, Instant};
#[cfg(windows)]
use support::*;

// region: The fixtures — a hook that starts something that outlives it
// ---------------------------------------------------------------------------
// The fixtures — a hook that starts something that outlives it
//
// Three variants of one script, from the plan's §6: A daemonizes and exits, A′
// daemonizes something that keeps *writing*, B daemonizes and then hangs past
// its own timeout. The grandchild reports two pids into a file — its own and
// its parent's, which is the direct child Emma spawned — because both are
// needed and neither is otherwise observable from out here.
// ---------------------------------------------------------------------------

/// The `start "" /b` line: a PowerShell grandchild that inherits the hook's
/// stdout (that inheritance is the entire defect), records the two pids, then
/// does whatever `tail` says.
///
/// `Get-CimInstance` rather than a hard-coded parent, because the direct child
/// is `cmd.exe` — Rust runs a `.cmd` through the shell by construction — and its
/// pid is not knowable from the test side.
#[cfg(windows)]
fn daemon_line(pidfile: &Path, tail: &str) -> String {
    // Forward slashes: the path is going inside a single-quoted PowerShell
    // string that is itself inside a double-quoted `cmd` argument, and a
    // backslash run is one escape too many for that stack to survive.
    let p = pidfile.display().to_string().replace('\\', "/");
    format!(
        "start \"\" /b powershell -NoProfile -Command \
         \"$p=(Get-CimInstance Win32_Process -Filter ('ProcessId='+$PID)).ParentProcessId; \
         [IO.File]::WriteAllText('{p}', ($p,$PID -join [char]10)); {tail}\""
    )
}

/// A grandchild that sleeps, holding the pipe and writing nothing.
#[cfg(windows)]
const SLEEPS: &str = "Start-Sleep -Seconds 20";

/// A grandchild that keeps *writing* into the abandoned pipe for ~20s.
///
/// The `try`/`catch` is the point rather than a convenience: after Emma drops
/// its read end the write fails, and what this asserts is that a writer which
/// declines to treat that as fatal simply carries on. On Windows there is no
/// signal to decline — a broken pipe is an error return — which is exactly the
/// claim `a_daemonizing_hooks_child_is_still_running_after_emma_walks_away`
/// exists to test rather than trust.
#[cfg(windows)]
const KEEPS_WRITING: &str = "for($i=0;$i -lt 200;$i++){ try { [Console]::Out.WriteLine('daemon'); \
                             [Console]::Out.Flush() } catch { }; Start-Sleep -Milliseconds 100 }";

/// Write the hook script and return the `hooks/…` path a spine entry names.
///
/// `body` is whatever the hook does between announcing itself and exiting; the
/// two `echo`s bracket it so a test can prove that output written *before* the
/// daemon was started and output written *after* both survive.
#[cfg(windows)]
fn leak_script(dir: &Path, name: &str, body: &str) -> String {
    std::fs::create_dir_all(dir).expect("mkdir hooks");
    let file = format!("{name}.cmd");
    let path = dir.join(&file);
    std::fs::write(
        &path,
        format!("@echo off\r\necho first line\r\n{body}\r\necho second line\r\n"),
    )
    .expect("write hook");
    format!("hooks/{file}")
}

/// A `.emma/` with one `UserPromptSubmit` hook in it.
///
/// That event rather than `PreToolUse` because it is the one where a hook's
/// plain stdout is *kept* — it becomes the context handed to the model — so a
/// test can assert on the bytes the defect was throwing away.
#[cfg(windows)]
fn harness_with(tag: &str, body: &str, timeout_ms: u64) -> Harness {
    let root = scratch(tag).join(".emma");
    let cmd = leak_script(&root.join("hooks"), "leak", body);
    write(
        &root.join("config.json"),
        &format!(
            r#"{{"hooks":{{"h":{{"event":"UserPromptSubmit","command":"{cmd}","timeout_ms":{timeout_ms}}}}}}}"#
        ),
    );
    Harness::load(&root).expect("a hook that exists loads")
}

/// The two pids the grandchild recorded: `(direct child, grandchild)`.
///
/// Polled rather than read once — `start` returns the instant the process is
/// created, and PowerShell needs a moment to reach its first statement. A test
/// that read the file immediately would be measuring PowerShell's start-up.
#[cfg(windows)]
fn pids(pidfile: &Path, within: Duration) -> (u32, u32) {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(pidfile) {
            let got: Vec<u32> = s
                .split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect();
            if got.len() == 2 {
                return (got[0], got[1]);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "the grandchild never recorded its pids in {}; the fixture did not stage the defect",
        pidfile.display()
    );
}

// endregion: The fixtures — a hook that starts something that outlives it

// region: Liveness, settled on a handle and never on a formatted string
// ---------------------------------------------------------------------------

#[cfg(windows)]
const SYNCHRONIZE: u32 = 0x0010_0000;

/// Is this process still running? A handle wait with a zero timeout, which is
/// the only answer that is about the process we opened rather than about a pid
/// that may since have been reissued.
#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
    unsafe {
        let h = OpenProcess(SYNCHRONIZE, 0, pid);
        if h.is_null() {
            return false;
        }
        let r = WaitForSingleObject(h, 0);
        CloseHandle(h);
        r != WAIT_OBJECT_0
    }
}

/// Wait up to `within` for a process to be gone, and say whether it is.
///
/// Bounded rather than instant because a kill is a *request*: the lesson this
/// discipline came from was a test that checked immediately after `taskkill`
/// and was flaky for a year.
#[cfg(windows)]
fn gone_within(pid: u32, within: Duration) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
    unsafe {
        let h = OpenProcess(SYNCHRONIZE, 0, pid);
        if h.is_null() {
            return true;
        }
        let r = WaitForSingleObject(h, within.as_millis() as u32);
        CloseHandle(h);
        r == WAIT_OBJECT_0
    }
}

/// Leave nothing behind. These fixtures deliberately create processes that
/// outlive the run, which is the guarantee — but a test suite that leaks twenty
/// PowerShells per run is its own defect.
#[cfg(windows)]
fn reap(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

// endregion: Liveness, settled on a handle and never on a formatted string

// region: The defect, and the fix
// ---------------------------------------------------------------------------

/// **Variant A: a hook that exited 0 is not reported as timed out.**
///
/// Red on the shipped code: `exec` read stdout to end-of-file before waiting on
/// the child, the grandchild held the write end for twenty seconds, and a hook
/// that had already exited 0 came back as `timed out after 3000ms` with its
/// output discarded. Green only if completion is keyed on the child's own exit.
///
/// Three assertions, and each one pins a different half of the fix: the
/// *duration* pins waiting on the child rather than on the pipe, the *exit code*
/// pins that the run is no longer a failure, and *both lines present* pins the
/// grace drain — the hook's output is still collected after the wait resolves.
#[cfg(windows)]
#[tokio::test]
async fn a_hook_that_exited_zero_is_not_reported_as_timed_out_because_a_grandchild_holds_the_pipe()
{
    let dir = scratch("exec-variant-a");
    let pidfile = dir.join("pids.txt");
    let harness = harness_with(
        "exec-variant-a-h",
        &daemon_line(&pidfile, SLEEPS),
        // Short on purpose: the grandchild outlives it by an order of
        // magnitude, so a pass cannot be the daemon quietly finishing first.
        3_000,
    );

    let started = Instant::now();
    let verdict = harness.on_user_prompt("hello", "sess-1", "t.jsonl").await;
    let waited = started.elapsed();
    let (child, grandchild) = pids(&pidfile, Duration::from_secs(10));

    let run = &verdict.runs[0];
    assert_eq!(
        run.exit_code,
        Some(0),
        "a hook that exited 0 was recorded as {:?}; stderr was {:?}",
        run.exit_code,
        run.stderr
    );
    assert!(
        !run.stderr.contains("timed out"),
        "the hook exited 0 in milliseconds and was reported as a timeout: {:?}",
        run.stderr
    );
    assert!(
        waited < Duration::from_millis(2_500),
        "waited {waited:?} on a hook that had already exited — completion is still keyed on \
         pipe EOF rather than on the child's exit"
    );
    let ctx = verdict.context.join("\n");
    assert!(
        ctx.contains("first line") && ctx.contains("second line"),
        "the hook's output did not survive: {ctx:?}"
    );

    reap(grandchild);
    let _ = child;
}

/// **Variant A′: the ruling, as a guarantee.**
///
/// A daemonizing hook is a supported use (owner ruling, 2026-08-14). This fails
/// if anyone reintroduces a job object, a process group kill or any other
/// tree-reaping — and it also fails if dropping Emma's read end were to kill the
/// daemon, which is the Windows claim the whole fix rests on: there is no
/// SIGPIPE, so a write into a broken pipe is an error return the writer may
/// ignore, and this grandchild is one that ignores it.
#[cfg(windows)]
#[tokio::test]
async fn a_daemonizing_hooks_child_is_still_running_after_emma_walks_away() {
    let dir = scratch("exec-variant-a-prime");
    let pidfile = dir.join("pids.txt");
    let harness = harness_with(
        "exec-variant-a-prime-h",
        &daemon_line(&pidfile, KEEPS_WRITING),
        3_000,
    );

    let verdict = harness.on_user_prompt("hello", "sess-1", "t.jsonl").await;
    assert_eq!(verdict.runs[0].exit_code, Some(0));
    let (_child, grandchild) = pids(&pidfile, Duration::from_secs(10));

    // Long enough that the daemon has attempted many writes into the pipe Emma
    // stopped reading. A death by broken pipe would have happened by now.
    std::thread::sleep(Duration::from_secs(2));
    let still_here = alive(grandchild);
    reap(grandchild);
    assert!(
        still_here,
        "the hook's daemon (pid {grandchild}) died after Emma dropped its read end. A \
         daemonizing hook is a supported use (owner ruling 2026-08-14): Emma abandons the pipe, \
         it does not reap the tree, and on Windows a broken pipe is an ignorable error rather \
         than a signal"
    );
}

/// **Variant B: the accepted cost, pinned so it reads as deliberate.**
///
/// A hook that hangs past its own timeout still gets *its own* process killed —
/// it broke the budget the operator set. What it started is not killed, and
/// cannot be: Emma has no way to tell a broken hook's orphans from the daemon
/// the operator meant to start, and under the ruling it must not guess. That is
/// a cost, not an oversight, and it is written down here so that the next reader
/// finds a test rather than a surprise.
#[cfg(windows)]
#[tokio::test]
async fn a_timed_out_hooks_descendants_survive_by_ruling_not_by_accident() {
    let dir = scratch("exec-variant-b");
    let pidfile = dir.join("pids.txt");
    let harness = harness_with(
        "exec-variant-b-h",
        // Daemonize, then hang far past the 1s budget below.
        &format!(
            "{}\r\nping -n 15 127.0.0.1 >nul",
            daemon_line(&pidfile, SLEEPS)
        ),
        1_000,
    );

    let verdict = harness.on_user_prompt("hello", "sess-1", "t.jsonl").await;
    let (child, grandchild) = pids(&pidfile, Duration::from_secs(10));

    assert!(
        verdict.runs[0].stderr.contains("timed out"),
        "a hook that hung past its budget was not reported as a timeout: {:?}",
        verdict.runs[0].stderr
    );
    // The direct child is killed, and the wait is bounded rather than instant
    // because a kill is a request rather than an event.
    let child_gone = gone_within(child, Duration::from_secs(5));
    let daemon_alive = alive(grandchild);
    reap(grandchild);
    assert!(
        child_gone,
        "the hook itself (pid {child}) outlived its timeout: kill_on_drop is not doing its job"
    );
    assert!(
        daemon_alive,
        "a timed-out hook's daemon (pid {grandchild}) was killed. Emma kills the child it \
         supervised and nothing else (owner ruling 2026-08-14)"
    );
}

/// **The grace drain is a drain, not a token gesture.**
///
/// Everything a hook writes has landed in the pipe buffer before `exit` can run,
/// so keying on the child's exit must not cost the last line. A hook that writes
/// a substantial block, daemonizes and exits gets all of it back — under the
/// timeout, with the write end still held open by something else.
#[cfg(windows)]
#[tokio::test]
async fn output_written_before_a_hook_daemonized_and_exited_is_all_collected() {
    let dir = scratch("exec-grace");
    let pidfile = dir.join("pids.txt");
    // 200 lines before the daemon starts and one after it: enough to be a real
    // drain of a real buffer rather than a single line that any implementation
    // would happen to have.
    let bulk = "for /l %%i in (1,1,200) do @echo line-%%i";
    let harness = harness_with(
        "exec-grace-h",
        &format!("{bulk}\r\n{}", daemon_line(&pidfile, SLEEPS)),
        3_000,
    );

    let started = Instant::now();
    let verdict = harness.on_user_prompt("hello", "sess-1", "t.jsonl").await;
    let waited = started.elapsed();
    let (_child, grandchild) = pids(&pidfile, Duration::from_secs(10));
    reap(grandchild);

    let ctx = verdict.context.join("\n");
    assert_eq!(verdict.runs[0].exit_code, Some(0), "{:?}", verdict.runs[0]);
    for expect in ["first line", "line-1", "line-200", "second line"] {
        assert!(ctx.contains(expect), "{expect} was lost from the drain");
    }
    assert!(
        waited < Duration::from_millis(2_500),
        "waited {waited:?}: the drain is unbounded again"
    );
}

// endregion: The defect, and the fix

// region: The status line gets the same behaviour, because it is the same code
// ---------------------------------------------------------------------------
// The status line gets the same behaviour, because it is the same code
//
// `StatusLine::run` calls `hooks::exec`, so the fix and the ruling reach it in
// the same commit rather than by anybody deciding they should. That is
// deliberate and it is what `statusline.rs`'s module doc argues for: the way to
// not have a second spawn site that behaves differently is to not have a second
// spawn site.
//
// The plan (§4) flags one half of this as wanting a *second* owner ruling, and
// it is not settled here: a status script runs on a debounced trigger many times
// a session, so one that daemonizes accumulates a process per burst in a way a
// once-per-event hook does not. Until that ruling exists there is one behaviour
// for both, and no caller-chosen policy — which is what this test pins.
// ---------------------------------------------------------------------------

/// A status script that daemonizes and exits is not reported as timed out, and
/// what it printed reaches the bottom row.
///
/// Before the fix this failed on *every debounced invocation*: the same false
/// receipt as the hook's, repeating several times a second, with the built-in
/// row shown forever in place of the operator's line.
#[cfg(windows)]
#[tokio::test]
async fn a_status_line_that_daemonizes_is_not_reported_as_timed_out_either() {
    let dir = scratch("exec-statusline");
    let pidfile = dir.join("pids.txt");
    let root = scratch("exec-statusline-h").join(".claude");
    let cmd = leak_script(&root.join("hooks"), "line", &daemon_line(&pidfile, SLEEPS));
    write(
        &root.join("settings.json"),
        &format!(r#"{{"statusLine":{{"type":"command","command":"{cmd}"}}}}"#),
    );
    let harness = Harness::load(&root).expect("boot");
    let line = harness.status_line().expect("configured");

    let started = Instant::now();
    let out = line
        .run(&emma_harness::StatusPayload::default(), 80, 24)
        .await;
    let waited = started.elapsed();
    let (_child, grandchild) = pids(&pidfile, Duration::from_secs(10));
    reap(grandchild);

    let out = out.expect("a status script that exited 0 must not report a failure");
    assert!(out.contains("first line"), "{out:?}");
    assert!(
        waited < line.timeout(),
        "the repaint path waited {waited:?} — its whole budget — on a script that had exited"
    );
}

// endregion: The status line gets the same behaviour, because it is the same code

// region: The grace, measured rather than assumed
// ---------------------------------------------------------------------------

/// **A hook's whole answer survives, however many reads it takes to collect.**
///
/// 35KB on stderr — five times the read chunk and past any pipe buffer, so the
/// hook blocks mid-write and the drain has to keep going round. A `drain` that
/// returned after one read, or a wait that stopped the collection at the child's
/// exit, loses most of it; the exact byte count is asserted because "contains
/// the last row" would also pass on a version that dropped a chunk from the
/// middle.
///
/// stderr rather than stdout on purpose: a `UserPromptSubmit` hook's stdout is
/// context, and context is separately capped at 10,000 characters
/// (`HOOK_CONTEXT_CAP`), so an assertion about the *pipe* made on stdout would
/// really be an assertion about that cap. That mistake cost an hour here.
#[cfg(windows)]
#[tokio::test]
async fn a_hooks_whole_answer_survives_however_many_reads_it_takes() {
    let dir = scratch("exec-manyreads");
    let big = dir.join("big.txt");
    let rows: String = (0..1000)
        .map(|i| format!("row-{i:04}-xxxxxxxxxxxxxxxxxxxxxxxx\r\n"))
        .collect();
    std::fs::write(&big, &rows).expect("write the blob");
    let harness = harness_with(
        "exec-manyreads-h",
        &format!("type {} 1>&2", big.display()),
        3_000,
    );

    let verdict = harness.on_user_prompt("hello", "sess-1", "t.jsonl").await;
    let seen = &verdict.runs[0].stderr;
    assert_eq!(
        seen.len(),
        rows.len(),
        "the hook wrote {} bytes and {} came back",
        rows.len(),
        seen.len()
    );
    assert!(seen.contains("row-0000") && seen.contains("row-0999"));
}

/// The plan's open question 1 (`notes/plans/process-lifetime.md` §8), run as an
/// experiment rather than answered from the armchair: a hook that writes its
/// last line and exits, 100 times, expecting zero lost lines.
///
/// **What it measured, and what it did not.** 0/100 lost — and 0/100 with
/// `EXEC_DRAIN_GRACE_MS` deleted as well, which is the honest result: on Windows
/// this race does not fire. Tokio gives a child's stdio overlapped I/O, so a
/// read is *posted* before the bytes exist and completes when they arrive; the
/// data is in our buffer before the exit notification is delivered, every time.
/// The grace is therefore unpinned on this platform — see the note in
/// `hooks::exec`. On unix, where the same code is readiness-based and the bytes
/// sit in the kernel until a poll collects them, this probe is expected to be
/// the one that catches a missing grace, and running it there is what would
/// settle the constant.
///
/// Kept `#[ignore]`d and kept honest: it is a probe that produced a number, not
/// a guard that would go red if the grace were removed.
#[cfg(windows)]
#[tokio::test]
#[ignore = "certification probe: 100 trials, ~1s, run by name"]
async fn certify_that_a_hooks_last_line_survives_its_exit_over_one_hundred_trials() {
    let harness = harness_with("exec-lastline", "rem nothing between the echoes", 3_000);
    let mut lost = 0;
    for _ in 0..100 {
        let verdict = harness.on_user_prompt("hello", "sess-1", "t.jsonl").await;
        let ctx = verdict.context.join("\n");
        if !(ctx.contains("first line") && ctx.contains("second line")) {
            lost += 1;
        }
    }
    println!("trials that lost a line: {lost}/100");
    assert_eq!(
        lost, 0,
        "{lost}/100 hook runs lost output written immediately before the hook exited"
    );
}

// endregion: The grace, measured rather than assumed

// region: The rate, because one trial is an anecdote
// ---------------------------------------------------------------------------

/// Twenty trials of variant A, reported as a rate.
///
/// `#[ignore]`d because it costs a minute of wall clock and stages twenty
/// daemons; run it by name when the numbers are the deliverable. This repository
/// has twice mistaken a red herring for a defect because somebody measured once,
/// so the shape of the evidence here is *n out of 20*, before and after.
#[cfg(windows)]
#[tokio::test]
#[ignore = "certification: twenty trials, ~1 minute, run by name"]
async fn certify_the_misreport_rate_over_twenty_trials() {
    let mut misreported = 0;
    let mut daemon_survived = 0;
    for i in 0..20 {
        let dir = scratch(&format!("exec-certify-{i}"));
        let pidfile = dir.join("pids.txt");
        let harness = harness_with(
            &format!("exec-certify-h-{i}"),
            &daemon_line(&pidfile, SLEEPS),
            3_000,
        );
        let verdict = harness.on_user_prompt("hello", "sess-1", "t.jsonl").await;
        let (_c, g) = pids(&pidfile, Duration::from_secs(10));
        if verdict.runs[0].stderr.contains("timed out") {
            misreported += 1;
        }
        if alive(g) {
            daemon_survived += 1;
        }
        reap(g);
    }
    println!("misreported as timed out: {misreported}/20; daemon survived: {daemon_survived}/20");
    assert_eq!(
        misreported, 0,
        "{misreported}/20 hooks that exited 0 were reported as timed out"
    );
    assert_eq!(
        daemon_survived, 20,
        "only {daemon_survived}/20 daemons survived"
    );
}

// endregion: The rate, because one trial is an anecdote

/// The unix arm is compile-checked on this box and nothing more, and that is
/// said here rather than in a report nobody will read next to the code. What a
/// Linux run would settle is in `notes/plans/process-lifetime.md` §6, and it is
/// not only "does it pass": closing the read end on unix *does* SIGPIPE a daemon
/// that later writes, so variant A′ is expected to behave differently there
/// unless the daemon ignores the signal — which is why the module doc on
/// `hooks::exec` tells a hook author to redirect a daemon's output.
///
/// **What is and is not left uncovered off Windows**, since a `cfg` that hides
/// a whole file is exactly the shape that quietly stops testing something. The
/// portable half of what this file asserts — a hook's exit is what completion
/// is keyed on, its stdout arrives whole, a hook that hangs is timed out rather
/// than waited for — is covered on unix by `harness_hooks.rs`, whose fixtures
/// are `#!/bin/sh` scripts with the executable bit set (its `script()` helper)
/// and which runs in full on both platforms. What has **no** unix test anywhere
/// is the case this file was built for: a hook that *daemonizes*, where the
/// grandchild inherits the pipe. That gap is real, it is the one thing the
/// ruling of 2026-08-14 turned into a guarantee, and it cannot be closed from a
/// Windows box — a fixture written here and never run would be a claim, not a
/// test.
#[cfg(not(windows))]
#[test]
fn the_process_lifetime_fixtures_are_windows_only_and_this_says_so() {
    let _ = support::scratch("exec-lifetime-note");
}
