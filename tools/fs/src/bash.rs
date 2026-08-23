//! `Bash` — a shell, on purpose, with a timeout and a cap.
//!
//! **Shell or argv?** A shell. The alternative — take an argv array and exec it
//! directly — is genuinely safer: no quoting, no word splitting, no injection
//! surface, no dependence on what `sh` happens to be. It is also unusable for
//! what this tool is for. Almost every command a coding agent needs to run is a
//! shell construct: `cargo test 2>&1 | tail -40`, `grep -c . file || true`,
//! `mkdir -p a && cd a`. With argv exec the model must decompose those itself,
//! and the failure mode is that it emits `sh -c "…"` as the argv anyway, which
//! is a shell with the safety story removed and nobody's caps applied.
//!
//! The injection argument also does not survive contact with the threat model.
//! There is no untrusted string being interpolated into a command here — the
//! model composes the whole command, and a model that wants to delete something
//! can do it just as easily through argv. What actually protects anything is
//! the approval gate reading `read_only: false`, which is why that field being
//! load-bearing matters more than the exec mechanism.
//!
//! What the shell choice does cost is honesty about containment: `cwd` is where
//! the command starts, not a wall it is held behind. `cd ..` leaves. That is
//! stated in the description rather than papered over, because a boundary the
//! operator believes in and that does not hold is worse than no boundary. The
//! `cwd` *argument* is contained — it must resolve inside the root — which is a
//! guard on the argument only, not on what the command then does with it.
//!
//! **Which shell, and why it is stated rather than searched for.** The search
//! used to be "the first `bash` on `PATH`". On a stock Windows box that is
//! `C:\Windows\System32\bash.exe`, the WSL launcher — and a real run showed it:
//! `pwd` came back `/mnt/e/Projects/emma` for a directory Emma had resolved and
//! contained as `C:\src\emma`, with `.wslconfig` warnings mixed into the
//! tool output. The two agreed only because WSL happened to translate the
//! directory. Measured on the same box, when it *cannot* translate — a `subst`
//! drive, a UNC path — WSL starts in the user's home directory instead, prints
//! nothing, and exits 0. That is a command running outside the root the
//! operator approved, reported as a success, which is a hole rather than
//! something the model can route around: nothing in the result says it
//! happened. So WSL is refused, with the reasoning, rather than translated for.
//!
//! The order, which is the contract:
//!
//! 1. `EMMA_SHELL`, if set to anything but whitespace — an absolute path, or
//!    one of `sh`, `bash`, `pwsh`, `powershell`. If what it names is not there,
//!    the call is `Unavailable` and says where it looked. It never falls back:
//!    a tool that runs a different shell than the one you asked for is worse
//!    than one that stops.
//! 2. `/bin/sh`, on unix.
//! 3. A native POSIX `bash` then `sh` — the Git for Windows locations first,
//!    then `PATH`, skipping the WSL launcher wherever it appears.
//! 4. Nothing. **PowerShell, WSL and `cmd.exe` are never chosen
//!    automatically.** PowerShell is a deliberate opt-in rather than a fallback
//!    because the model writes POSIX — pipes, `2>&1`, `&&` — on the strength of
//!    being told it has a shell, and Windows PowerShell 5.1 has no `&&` at all.
//!    Auto-selecting it would turn every such command into a wrong answer
//!    shaped like the command's own output. Named explicitly it works fine, and
//!    the model is told what it has.
//!
//! Told how? **In the result, not in the description.** The description is
//! hashed into `tool_schema_hash`, which is how anyone tells which tool surface
//! produced a given answer; a description that named the local shell would make
//! that hash a property of the machine. So the description says one thing true
//! everywhere, and every result carries `shell: <kind> — <path>` on its first
//! line. The model learns exactly what it is driving, at run time, and adapts —
//! which is what it is good at, and cheaper than any compatibility shim.
//!
//! **Exit status is not failure.** A command that started and finished is `Ok`,
//! carrying `exit status <n>` directly under the shell banner; only
//! could-not-spawn, timed-out and killed are `Failed`. The reasoning is recorded
//! at the point in `run` that implements it, below.
//!
//! The contract is stated in three places — here, in `run`, and in
//! `descriptions/bash.md`, which is the only one the model reads — and for a
//! while it was stated three different ways. The description went on describing
//! a non-zero exit as a failure long after the code stopped treating it as one,
//! because changing it moves the registry's schema hash and so kept being
//! deferred as too deliberate for a drive-by edit. That deferral cost more than
//! the edit would have: it taught the model to avoid `Bash` for exactly the
//! commands it should use it for, and to append `|| true`, which discards the
//! status the tool now reports correctly. All three agree as of 2026-08-10.
//! **A contract change is not landed until it has reached the copy the model
//! is shown.**
//!
//! The description moved once more the same day, for the same class of reason:
//! it promised `sh -c`, which is false under PowerShell and false about `&&` in
//! particular. It now says the shell varies and that the result names it. That
//! moves `tool_schema_hash` — deliberately, once, to wording true on every
//! platform, which is what keeps the hash a property of the tool surface rather
//! than of the box it happened to run on.
//!
//! **Background mode.** `run_in_background: true` spawns the same child the
//! foreground path would — same shell resolution, same rebuilt environment,
//! same contained `cwd` — and returns at once with a task id instead of
//! waiting. The child's lifetime, output and exit move to
//! `ToolCtx::background`, where `BashOutput` and `KillShell` find them. The
//! reasoning about what the two paths must share and where they must differ is
//! on [`command_for`] and in the background region below.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use emma_tool_api::background::{Task, TaskState};
use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use crate::args;
use crate::path;

// region: The tool surface
// ---------------------------------------------------------------------------
// The tool surface
//
// The caps, the environment allowlist, and what the model is shown and allowed
// to say. Everything here is decidable without touching the filesystem, which
// is why `validate_args` can live in it.
// ---------------------------------------------------------------------------

const NAME: &str = "Bash";
const KEYS: &[&str] = &["command", "timeout_ms", "cwd", "run_in_background"];

pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
pub const MAX_TIMEOUT_MS: u64 = 600_000;
pub const MAX_STREAM_BYTES: usize = 64 * 1024;

/// Everything the child is allowed to inherit. Cleared and rebuilt rather than
/// filtered, so a variable added to the parent's environment later — an API key
/// exported by whatever launched Emma — cannot appear here by default.
const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "TZ",
    "TERM",
];

#[derive(Default)]
pub struct Bash;

impl Bash {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for Bash {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("descriptions/bash.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command line. The shell that ran it is named on the first line of the result." },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!("Default {DEFAULT_TIMEOUT_MS}, capped at {MAX_TIMEOUT_MS}.")
                },
                "cwd": {
                    "type": "string",
                    "description": "Directory to start in. Must be inside the working directory. Defaults to it."
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Run the command without waiting for it. The result names a task id; the command has NOT finished when the call returns and no output or exit status is included. Read output later with BashOutput; stop it with KillShell. timeout_ms is refused alongside this — a background command runs until it exits or is killed."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: false,
            // False, and this is the one declaration in the workspace that
            // wants its argument attached: `Bash` can obviously `curl`. It
            // says false because the axis is about tools whose purpose is
            // egress, and because `read_only: false` already gates every call
            // here behind a prompt that shows the command itself — which is
            // strictly more information than a host name. Declaring true would
            // oblige `network_target` to name a destination, and naming one
            // means parsing shell to find it: an arms race with every quoting
            // trick there is, losing quietly. See `ToolMeta::reaches_network`.
            reaches_network: false,
            idempotent: false,
        }
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, NAME, KEYS)?;
        let command = args::req_str(args_v, NAME, "command")?;
        if command.trim().is_empty() {
            return Err(ToolError::BadArguments("Bash.command is empty".into()));
        }
        args::opt_str(args_v, NAME, "cwd")?;
        if let Some(0) = args::opt_u64(args_v, NAME, "timeout_ms")? {
            return Err(ToolError::BadArguments(
                "Bash.timeout_ms must be at least 1".into(),
            ));
        }
        // Refused rather than accepted-and-ignored. A background call has no
        // wait for a timeout to bound, so honouring the argument is impossible,
        // and swallowing it would leave the caller believing a fuse is lit
        // when nothing is armed — the configuration-that-lies class this
        // repository has been bitten by twice. `deny_unknown` above already
        // refuses a key that does nothing; this is the same rule applied to a
        // known key that does nothing in this combination.
        if args::opt_bool(args_v, NAME, "run_in_background")?.unwrap_or(false)
            && args_v.get("timeout_ms").is_some()
        {
            return Err(ToolError::BadArguments(
                "Bash.timeout_ms has no effect on a background task, so the pair is \
                 refused rather than half-honoured: a background command runs until it \
                 exits or KillShell stops it. Drop timeout_ms, or drop run_in_background."
                    .into(),
            ));
        }
        Ok(())
    }

    async fn invoke(
        &self,
        ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(ctx, args_v).await)
    }
}

// endregion: The tool surface

// region: Running the command
// ---------------------------------------------------------------------------
// Running the command
//
// Spawn, drain, wait, and then decide what the result was. The ordering in here
// is load-bearing in three places — drains started before the wait, the kill
// bounded, and the exit-status ruling at the end — and each is commented where
// it happens.
// ---------------------------------------------------------------------------

impl Bash {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let root = path::root(ctx)?;
        let command = args::req_str(&args_v, NAME, "command")?;
        // An over-large timeout is clamped rather than refused. Zero is refused
        // in `validate_args`, because zero is a mistake with no plausible
        // meaning, whereas "wait an hour" is a real intention the engine simply
        // will not honour past its own ceiling.
        let timeout_ms = args::opt_u64(&args_v, NAME, "timeout_ms")?
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS);

        let cwd = match args::opt_str(&args_v, NAME, "cwd")? {
            None => root.clone(),
            Some(p) => {
                let (resolved, meta) = path::resolve_existing(&root, p)?;
                if !meta.is_dir() {
                    return Err(ToolError::BadArguments(format!("{p} is not a directory")));
                }
                resolved
            }
        };

        let shell = resolve_shell()?;
        let mut cmd = command_for(&shell, command, &cwd);

        if args::opt_bool(&args_v, NAME, "run_in_background")?.unwrap_or(false) {
            return run_background(ctx, cmd, &shell, command);
        }

        // Without this a timed-out command keeps running after Emma has given
        // up on it, and the next call sees a machine still busy with work
        // nobody is waiting for. Set here rather than in `command_for` because
        // it is the one setting on which the two paths need opposite answers —
        // a background child's whole point is outliving the call.
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| {
            ToolError::Unavailable(format!(
                "{} could not be started: {e}",
                shell.path.display()
            ))
        })?;
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        // Drained concurrently with the wait, and drained *past* the cap rather
        // than stopping: a reader that stops reading fills the pipe buffer and
        // the child blocks forever on a write nobody consumes, so the timeout
        // becomes the only exit and every noisy command takes the full 120s.
        let drains = tokio::spawn(async move {
            tokio::join!(
                drain(stdout, MAX_STREAM_BYTES),
                drain(stderr, MAX_STREAM_BYTES)
            )
        });

        let waited = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await;
        let timed_out = waited.is_err();
        let status = match waited {
            Ok(Ok(status)) => Some(status),
            Ok(Err(e)) => {
                return Err(ToolError::Failed(format!(
                    "the command could not be run: {e}"
                )))
            }
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                None
            }
        };

        // Bounded again: a grandchild holding the pipe open outlives the kill,
        // and waiting on it forever would turn a timeout into a hang.
        // Losing the race here discards whatever had been drained and reports
        // both streams as cut. That loses output, which is the lesser harm: the
        // alternative is a `Bash` call that never returns, and "cut" is at
        // least true of a result assembled from nothing.
        let (drained, ((out, out_cut), (err, err_cut))) =
            match tokio::time::timeout(Duration::from_secs(5), drains).await {
                Ok(Ok(pair)) => (true, pair),
                _ => (false, ((Vec::new(), true), (Vec::new(), true))),
            };

        let body = render(&out, &err);
        let cut = out_cut || err_cut;

        // A timeout is one of the three genuine failures — the command did not
        // finish, so there is no status to report and nothing answered. The
        // output produced before the kill still goes in the message: a build
        // that hung after printing where it hung is telling you where it hung.
        if timed_out {
            return Err(ToolError::Failed(format!(
                "the command was killed after {timeout_ms}ms.\n{}\n{body}",
                shell.banner()
            )));
        }

        // A command that ran and exited non-zero is **not** a tool failure.
        //
        // The contract's rule — an error is a fact about the call, never about
        // what the world contains — was written about the filesystem, and it
        // did not survive first contact with exit-status-as-answer. `grep -q`
        // answering "no" exits 1. `test -f` answering "it is not there" exits
        // 1. `cargo test` answering "three of these fail" exits 101. Every one
        // of those is the world answering the question that was asked, and
        // reporting them as `Failed` tells the model its shell is broken and
        // invites it to route around a problem it does not have.
        //
        // So the line is drawn at whether the command ran: could not spawn,
        // timed out, or was killed is `Failed`; anything that started and
        // finished is `Ok`, with the status stated in the content because the
        // model's next move depends on both the number and what was printed.
        //
        // Resolved this way rather than as "Bash is the documented exception",
        // because "except X" is how a rule stops being enforceable.
        let status = status.expect("status present when not timed out");
        let code = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "a signal".into());

        let mut content = body;
        if !status.success() {
            // Stated first: a model that skims sees the outcome before the
            // output, and a non-zero exit changes how the output should be read.
            content = format!("exit status {code}\n{content}");
        }
        // Above the status, on every call, because the model is not told which
        // shell it has anywhere else — see `Shell::banner`. Every call rather
        // than once per session: there is no session state here to hang "once"
        // on, a resumed or compacted conversation would lose the one mention,
        // and the cost is one short line against a 64 KiB cap.
        content = format!("{}\n{content}", shell.banner());
        // Two different cuts wearing one flag until now, and they call for
        // opposite moves. The cap means the command printed more than fits and
        // the output is a prefix of what it said; losing the drain race means
        // whatever had been read was thrown away, so the result is assembled
        // from nothing and the *whole* output is missing. Telling the model
        // "cut at 65536 bytes" in the second case names a limit that never
        // fired, and it would trust the empty body as short output.
        let reason = if !drained {
            "the command's output was discarded: its streams could not be drained within 5s \
             of it exiting, which happens when a background grandchild still holds the pipe \
             open. Nothing here is the command's output and no argument changes that — \
             re-run it redirecting output to a file, then Read or Grep the file."
                .to_string()
        } else {
            format!(
                "output cut at the fixed {MAX_STREAM_BYTES}-byte per-stream cap, which no \
                 argument raises; what is shown is the start of each stream. Re-run redirecting \
                 output to a file and Read or Grep the file for the rest, or narrow the command \
                 so it prints less."
            )
        };
        if cut {
            content.push_str(&format!("\n[truncated: {reason}]"));
        }
        // Structural, alongside the sentence in `content`. The footer reads
        // this; the model reads the prose. Set for every command that ran,
        // including a successful one, because "exited 0" and "never ran" are
        // different facts and the footer has to tell them apart.
        // Structural, alongside the sentence in `content`. The footer reads
        // this; the model reads the prose. Recorded for every command that ran,
        // including a successful one, because "exited 0" and "never ran" are
        // different facts and the footer has to tell them apart. A command
        // killed by a signal has no numeric code and stays `None` — the same
        // "no number to report" the field already means.
        let outcome = match status.code() {
            Some(c) => ToolOutcome::new(content).with_exit_code(i64::from(c)),
            None => ToolOutcome::new(content),
        };
        Ok(if cut {
            outcome.truncated_because(reason)
        } else {
            outcome
        })
    }
}

/// One construction of the child for both the foreground and background paths.
///
/// Factored so the two call sites cannot drift, because the parts a review
/// would not notice missing from one of them — `env_clear` plus the rebuilt
/// allowlist, the resolved and contained `cwd`, the null stdin — are exactly
/// the parts carrying the security story. A background path that quietly
/// inherited the parent environment would hand every child the API key and
/// pass every foreground test while doing it.
///
/// What is deliberately *not* here is `kill_on_drop`: it is the one setting on
/// which the paths need opposite answers, so each states its own at its own
/// call site rather than one inheriting the other's by default.
fn command_for(shell: &Shell, command: &str, cwd: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&shell.path);
    cmd.args(shell.args())
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for key in ENV_ALLOWLIST {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
    cmd
}

// endregion: Running the command

// region: Running in the background
// ---------------------------------------------------------------------------
// Running in the background
//
// Spawn, register, return. The call ends immediately; the child, its output
// and its exit live in `ToolCtx::background` from then on, where `BashOutput`
// and `KillShell` find them by the id this returns.
// ---------------------------------------------------------------------------

/// How often the background waiter checks whether `KillShell` fired.
///
/// A poll rather than a channel because `Child::start_kill` needs the child,
/// which lives inside the detached task, and this crate's tokio carries no
/// `sync` feature to send a message into it with. A tenth of a second of kill
/// latency is imperceptible next to the teardown it triggers, and the timer
/// only ticks while a background task is running.
const KILL_POLL: Duration = Duration::from_millis(100);

/// How long the final state waits for the pipes to reach EOF after the exit.
///
/// The same 5s bound the foreground path puts on the same race, for the same
/// grandchild-holds-the-pipe reason — with a gentler failure: past the bound
/// the pumps keep draining into the registry, so a late tail still arrives for
/// whoever reads next, and only the state stops waiting for it.
const PUMP_GRACE: Duration = Duration::from_secs(5);

/// The background half of `run`. Returns as soon as the child exists.
fn run_background(
    ctx: &ToolCtx,
    mut cmd: tokio::process::Command,
    shell: &Shell,
    command: &str,
) -> Result<ToolOutcome, ToolError> {
    // `kill_on_drop` stays off — the whole feature hangs on that. The
    // foreground sets it so a timed-out command cannot outlive the call;
    // here outliving the call is the point, and with it on, any drop of the
    // child handle short of a completed wait — the runtime tearing down, the
    // waiter task aborted — would silently take the command with it while the
    // registry went on saying `Running` about a process that no longer exists.
    let mut child = cmd.spawn().map_err(|e| {
        ToolError::Unavailable(format!(
            "{} could not be started: {e}",
            shell.path.display()
        ))
    })?;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    // Registered only after the spawn succeeded: an entry for a child that
    // never existed would be a task forever `Running` that nothing can finish
    // or kill. A failed spawn is a `ToolError` like the foreground's — an
    // observation the loop continues past, not an abort.
    let task = ctx.background.spawn(&ctx.session_id, command);

    let kill = Arc::new(AtomicBool::new(false));
    {
        let kill = kill.clone();
        task.on_kill(move || kill.store(true, Ordering::SeqCst));
    }

    let watcher = task.clone();
    let id = task.id.clone();
    tokio::spawn(async move {
        // Both pipes are pumped from the moment of spawn, and the pump never
        // stops reading: the cap on what is *kept* lives in `Task::push`,
        // which drops the oldest bytes, so the reader's only job is keeping
        // the pipe empty. A reader that stopped would fill the pipe buffer
        // and block the child forever on a write nobody consumes — the same
        // deadlock the foreground `drain` documents, one layer over.
        let out_pump = tokio::spawn(pump(stdout, watcher.clone()));
        let err_pump = tokio::spawn(pump(stderr, watcher.clone()));

        let status = loop {
            tokio::select! {
                s = child.wait() => break s,
                _ = tokio::time::sleep(KILL_POLL) => {
                    if kill.load(Ordering::SeqCst) {
                        // The shell, not its descendants. Emma does not reap
                        // process trees — the owner ruled a deliberately
                        // daemonizing child is a supported use — and
                        // `Task::kill` is honest about that blast radius.
                        let _ = child.start_kill();
                    }
                }
            }
        };

        // The final state waits for the pipes, bounded. Unwaited, the state
        // could land before the last buffered bytes and a poller that stops
        // at `finished()` would miss the tail it was promised — `read_new`
        // couples state and output under one lock for exactly that reader.
        // Unbounded, a grandchild holding the pipe open would keep a command
        // that exited hours ago `Running` forever.
        let _ = tokio::time::timeout(PUMP_GRACE, async {
            let _ = out_pump.await;
            let _ = err_pump.await;
        })
        .await;

        // `Task::kill` records `Killed` itself; writing `Exited` over it
        // would report a command the user stopped as one that finished.
        if kill.load(Ordering::SeqCst) {
            return;
        }
        match status {
            Ok(st) => watcher.set_state(TaskState::Exited(st.code())),
            Err(e) => watcher.set_state(TaskState::Failed(format!(
                "the wait on the child failed: {e}"
            ))),
        }
    });

    // The outcome deliberately over-explains that nothing has finished. A
    // model that reads a background spawn as a completed command reports
    // success for work that has not happened, so the wording denies it every
    // handle that conclusion could hang on: no exit code is recorded on the
    // outcome, and the content says what has and has not occurred.
    Ok(ToolOutcome::new(format!(
        "{}\nstarted in the background as task {id}. The command has not finished: \
         nothing has been waited for, no exit status exists yet, and any output it has \
         produced so far is not shown here. Call BashOutput with this task id to read \
         its output and, once it exits, its status; call KillShell with the same id to \
         stop it.",
        shell.banner()
    )))
}

/// Feed one pipe into the task's buffer until EOF.
///
/// No cap parameter, on purpose: `Task::push` retains at most the registry's
/// cap and reports what it dropped, so this loop reads unconditionally. The
/// foreground `drain` keeps the head because its call returns once and the
/// first error is the real one; a polled task wants the newest bytes, and the
/// registry's oldest-first eviction already chooses that.
async fn pump<R: tokio::io::AsyncRead + Unpin>(mut reader: R, task: Task) {
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => task.push(&chunk[..n]),
        }
    }
}

// endregion: Running in the background

// region: Output
// ---------------------------------------------------------------------------
// Output
//
// How the two streams are turned into one readable body, and how they are read
// without deadlocking the child. Choosing the shell used to live here too; it
// outgrew the corner and is its own region below.
// ---------------------------------------------------------------------------

/// stderr is labelled rather than interleaved. Interleaving is what a terminal
/// shows and it is not reconstructible here — the two pipes arrive
/// independently — so a merged view would invent an ordering that did not
/// happen. Both empty is `(no output)` rather than an empty string, because a
/// command that printed nothing and a call that lost its output should not read
/// the same.
fn render(out: &[u8], err: &[u8]) -> String {
    let out = String::from_utf8_lossy(out);
    let err = String::from_utf8_lossy(err);
    match (out.trim().is_empty(), err.trim().is_empty()) {
        (true, true) => "(no output)".to_string(),
        (false, true) => out.into_owned(),
        (true, false) => format!("--- stderr ---\n{err}"),
        (false, false) => format!("{out}\n--- stderr ---\n{err}"),
    }
}

/// Reads to end of stream and keeps only the first `cap` bytes.
///
/// The `continue` past the cap is the point of the whole function: it keeps
/// consuming and discarding rather than returning. Stopping would leave the
/// pipe buffer full and the child blocked forever on a write nobody is reading,
/// so every noisy command would run to its full timeout instead of finishing.
/// Keeping the *head* rather than the tail is the deliberate half of that — the
/// first error is usually the real one, and the rest is cascade.
async fn drain<R: tokio::io::AsyncRead + Unpin>(mut reader: R, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut cut = false;
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if kept.len() >= cap {
                    cut = true;
                    continue;
                }
                let room = cap - kept.len();
                let take = room.min(n);
                kept.extend_from_slice(&chunk[..take]);
                cut |= take < n;
            }
        }
    }
    (kept, cut)
}

// endregion: Output

// region: Which shell
// ---------------------------------------------------------------------------
// Which shell
//
// A stated order and an explicit override, rather than a scan that takes
// whatever exists first. The order is in the module doc because a user has to
// be able to predict it without reading this; what is here is the mechanism
// and the two refusals.
// ---------------------------------------------------------------------------

/// The override. An environment variable rather than a config key because it
/// needs no plumbing, is settable per-run without editing a file, and is the
/// same shape as `SHELL` and `CHROME` — the things it sits next to.
///
/// Deliberately absent from [`ENV_ALLOWLIST`]: it selects Emma's shell and has
/// no business being visible to the commands that shell runs.
pub const OVERRIDE_ENV: &str = "EMMA_SHELL";

/// Where a POSIX shell lives on Windows when somebody installed one on purpose.
/// Searched *before* `PATH`, which is the whole fix: WSL's launcher is on
/// `PATH` ahead of Git for Windows on a stock box, so ordering by `PATH` alone
/// picks the one shell whose filesystem Emma cannot reason about.
const WINDOWS_POSIX_DIRS: &[(&str, &str)] = &[
    ("ProgramFiles", r"Git\bin"),
    ("ProgramW6432", r"Git\bin"),
    ("ProgramFiles(x86)", r"Git\bin"),
    ("LOCALAPPDATA", r"Programs\Git\bin"),
    ("ProgramFiles", r"Git\usr\bin"),
];

/// Directory names Windows keeps the WSL launcher in. `bash.exe` in any of them
/// is the interop stub, not a shell — see [`wsl_refusal`].
const WSL_DIRS: &[&str] = &["system32", "syswow64", "sysnative", "windowsapps"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Posix,
    PowerShell,
    Wsl,
}

impl ShellKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Posix => "posix",
            Self::PowerShell => "powershell",
            Self::Wsl => "wsl",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellSource {
    Override,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    pub path: PathBuf,
    pub kind: ShellKind,
    pub source: ShellSource,
}

impl Shell {
    /// The arguments before the command line.
    ///
    /// `-Command` spelled out rather than `-c`. Both work — that was checked
    /// on Windows PowerShell 5.1 rather than assumed, after an earlier comment
    /// here confidently claimed 5.1 rejects the short form — but `-c` is an
    /// abbreviation resolved against every parameter starting with `c`, and it
    /// stays unambiguous only as long as nobody adds another one.
    ///
    /// The two that are load-bearing: `-NoProfile`, because a user's profile
    /// is arbitrary code that would otherwise run before every tool call —
    /// slow, and a source of state that no cap or allowlist here covers; and
    /// `-NonInteractive`, so a cmdlet that wants to prompt fails instead of
    /// waiting on a stdin that is `/dev/null` until the timeout.
    fn args(&self) -> &'static [&'static str] {
        match self.kind {
            ShellKind::Posix => &["-c"],
            ShellKind::PowerShell => &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"],
            // Never reached: `resolve_with` refuses before constructing one.
            ShellKind::Wsl => &["-c"],
        }
    }

    /// The one line prepended to every result.
    ///
    /// This exists because the *description* cannot say it. The description is
    /// hashed into `tool_schema_hash`, which is how anyone tells which tool
    /// surface produced a given answer; a description that named the local
    /// shell would make that hash a property of the machine and the attribution
    /// would stop meaning anything. So the description stays true everywhere
    /// and says nothing machine-specific, and the machine-specific fact arrives
    /// here, in the result, where it is exact and costs forty bytes.
    pub fn banner(&self) -> String {
        format!("shell: {} — {}", self.kind.label(), self.path.display())
    }
}

impl std::fmt::Display for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = match self.source {
            ShellSource::Override => OVERRIDE_ENV,
            ShellSource::Default => "default",
        };
        write!(
            f,
            "{} shell at {} ({source})",
            self.kind.label(),
            self.path.display()
        )
    }
}

/// Everything the decision depends on, passed in rather than read.
///
/// The point is that the order and both refusals are then a pure function of a
/// directory list and an "does this exist" predicate, so every branch is
/// testable on any box without a shell installed and without the answer
/// depending on what this particular machine has.
struct ShellEnv<'a> {
    windows: bool,
    /// Searched before `PATH`. See [`WINDOWS_POSIX_DIRS`].
    preferred_dirs: Vec<PathBuf>,
    path_dirs: Vec<PathBuf>,
    exists: &'a dyn Fn(&Path) -> bool,
}

/// The resolved shell, or an honest refusal — against the real environment.
///
/// Public because "which shell ran my command" must be answerable without
/// reading this file. `Bash` calls it per invocation; a `/shell`-style command
/// elsewhere in the harness can call the same function and print
/// `Shell::to_string()`.
pub fn resolve_shell() -> Result<Shell, ToolError> {
    let exists = |p: &Path| p.is_file();
    let env = ShellEnv {
        windows: cfg!(windows),
        preferred_dirs: windows_posix_dirs(),
        path_dirs: std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default(),
        exists: &exists,
    };
    resolve_with(std::env::var(OVERRIDE_ENV).ok().as_deref(), &env)
}

fn windows_posix_dirs() -> Vec<PathBuf> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (var, suffix) in WINDOWS_POSIX_DIRS {
        if let Some(base) = std::env::var_os(var) {
            let dir = PathBuf::from(base).join(suffix);
            if !out.contains(&dir) {
                out.push(dir);
            }
        }
    }
    out
}

/// What a path *is*, from its name alone.
///
/// Name-based rather than probe-based on purpose: this runs before anything is
/// spawned, and a decision that required executing the candidate to find out
/// what it was would have already run it.
fn classify(path: &Path) -> ShellKind {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if stem == "pwsh" || stem == "powershell" {
        return ShellKind::PowerShell;
    }
    if stem == "wsl" {
        return ShellKind::Wsl;
    }
    let parent = path
        .parent()
        .map(|p| p.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let in_system_dir = parent
        .rsplit(['\\', '/'])
        .next()
        .is_some_and(|leaf| WSL_DIRS.contains(&leaf));
    if in_system_dir {
        return ShellKind::Wsl;
    }
    ShellKind::Posix
}

/// The refusal that carries the reasoning, because "no" without "why" here
/// sends people to the one workaround that reopens the hole.
///
/// WSL is a separate filesystem namespace. Emma resolves and contains `cwd` as
/// a Windows path; the shell would receive a directory it can only see through
/// a translation Emma is not doing. When that translation succeeds the two
/// agree by luck; when it fails — measured on this box with a `subst` drive and
/// with a UNC path — WSL starts in the user's home directory instead, prints no
/// warning and exits 0. That is a command running outside the root the operator
/// approved, reported as a success. It is a hole rather than something the
/// model can route around, because nothing in the result says it happened.
fn wsl_refusal(path: &Path) -> ToolError {
    ToolError::Unavailable(format!(
        "{} is the WSL launcher, and Emma will not run commands through it. WSL is a \
         separate filesystem namespace: Emma resolves and contains `cwd` as a Windows \
         path, and when WSL cannot translate the directory it was started in it silently \
         starts in the user's home directory instead — so containment would mean one \
         thing to Emma and another to the shell. Install Git for Windows (Emma looks in \
         Program Files\\Git\\bin) or set {OVERRIDE_ENV} to a native shell, or run Emma \
         itself inside WSL, where the root is a WSL path and containment means one thing \
         again.",
        path.display()
    ))
}

/// The documented order, in one function.
///
/// 1. [`OVERRIDE_ENV`], if set to anything but whitespace.
/// 2. `/bin/sh` on unix.
/// 3. A native POSIX `bash`/`sh`: the Git for Windows locations, then `PATH`.
/// 4. Nothing. PowerShell, WSL and `cmd.exe` are never chosen automatically.
fn resolve_with(over: Option<&str>, env: &ShellEnv<'_>) -> Result<Shell, ToolError> {
    if let Some(spec) = over.map(str::trim).filter(|s| !s.is_empty()) {
        return resolve_override(spec, env);
    }
    if !env.windows {
        let sh = PathBuf::from("/bin/sh");
        if (env.exists)(&sh) {
            return Ok(default_shell(sh));
        }
    }
    // Only ever `bash` and `sh`. `cmd.exe` is not a fallback for the same
    // reason WSL is not: a command written for `sh` run under a shell with
    // different quoting, globbing and operators produces wrong answers that
    // look like the command's own output.
    for name in names_for(if env.windows { "bash" } else { "sh" }, env) {
        for dir in env.preferred_dirs.iter().chain(env.path_dirs.iter()) {
            let candidate = dir.join(&name);
            // Skipped rather than merely ranked below Git bash: on a box
            // without Git, ranking alone still ends at WSL.
            if (env.exists)(&candidate) && classify(&candidate) == ShellKind::Posix {
                return Ok(default_shell(candidate));
            }
        }
    }
    if env.windows {
        for name in names_for("sh", env) {
            for dir in env.preferred_dirs.iter().chain(env.path_dirs.iter()) {
                let candidate = dir.join(&name);
                if (env.exists)(&candidate) && classify(&candidate) == ShellKind::Posix {
                    return Ok(default_shell(candidate));
                }
            }
        }
    }
    Err(nothing_found(env))
}

fn default_shell(path: PathBuf) -> Shell {
    let kind = classify(&path);
    Shell {
        path,
        kind,
        source: ShellSource::Default,
    }
}

fn resolve_override(spec: &str, env: &ShellEnv<'_>) -> Result<Shell, ToolError> {
    // A bare `wsl` is refused before any lookup, so the message is the same
    // whether or not the launcher happens to be installed.
    if spec.eq_ignore_ascii_case("wsl") {
        return Err(wsl_refusal(Path::new("wsl")));
    }
    let looks_like_path = spec.contains(['/', '\\']) || Path::new(spec).is_absolute();
    let found = if looks_like_path {
        let p = PathBuf::from(spec);
        (env.exists)(&p).then_some(p)
    } else {
        names_for(spec, env)
            .into_iter()
            .flat_map(|name| {
                env.preferred_dirs
                    .iter()
                    .chain(env.path_dirs.iter())
                    .map(move |d| d.join(&name))
            })
            .find(|c| (env.exists)(c))
    };
    let Some(path) = found else {
        // Refuse rather than fall back. A tool that silently runs a shell other
        // than the one you named is worse than one that stops: the command
        // still produces output, and the output is wrong in a way that reads as
        // the command's own answer.
        let where_looked = if looks_like_path {
            "no file at that path".to_string()
        } else {
            format!("looked in: {}", dir_list(env))
        };
        return Err(ToolError::Unavailable(format!(
            "{OVERRIDE_ENV} names \"{spec}\", and no such shell was found ({where_looked}). \
             Set {OVERRIDE_ENV} to an absolute path, or to one of: sh, bash, pwsh, \
             powershell — or unset it to take the default."
        )));
    };
    let kind = classify(&path);
    if kind == ShellKind::Wsl {
        return Err(wsl_refusal(&path));
    }
    Ok(Shell {
        path,
        kind,
        source: ShellSource::Override,
    })
}

/// Filenames to try for a shell named `name`, `.exe` included on Windows.
fn names_for(name: &str, env: &ShellEnv<'_>) -> Vec<String> {
    let mut out = vec![name.to_string()];
    if env.windows {
        let with_exe = format!("{name}.exe");
        out.insert(0, with_exe);
    }
    out
}

fn dir_list(env: &ShellEnv<'_>) -> String {
    let dirs: Vec<String> = env
        .preferred_dirs
        .iter()
        .chain(env.path_dirs.iter())
        .map(|d| d.display().to_string())
        .collect();
    if dirs.is_empty() {
        "no directories to search".into()
    } else {
        dirs.join(", ")
    }
}

/// The refusal when the search comes up empty. It names what was looked for and
/// where, and both opt-ins, because the alternative is a user reconstructing
/// this function from the outside.
fn nothing_found(env: &ShellEnv<'_>) -> ToolError {
    let what = if env.windows {
        "bash.exe, sh.exe"
    } else {
        "sh"
    };
    ToolError::Unavailable(format!(
        "no POSIX shell is available. Emma looked for {what} in: {}. Set {OVERRIDE_ENV} to \
         the absolute path of the shell you want. PowerShell is never chosen automatically \
         — {OVERRIDE_ENV}=powershell (or pwsh) selects it, and the shell that ran is named \
         on the first line of every Bash result.",
        dir_list(env)
    ))
}

// endregion: Which shell

// region: Resolution tests
// ---------------------------------------------------------------------------
// Resolution tests
//
// The order, the override and the refusals are decided from a list of
// directories and an "does this exist" predicate, so all of it is testable
// without a shell installed and without the answer depending on this box. The
// tests that genuinely need a shell to run are in `tests/edges.rs`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod shell_tests {
    use super::*;

    /// A fake filesystem: only these paths exist.
    fn env<'a>(
        windows: bool,
        preferred: &[&str],
        path: &[&str],
        exists: &'a dyn Fn(&Path) -> bool,
    ) -> ShellEnv<'a> {
        ShellEnv {
            windows,
            preferred_dirs: preferred.iter().map(PathBuf::from).collect(),
            path_dirs: path.iter().map(PathBuf::from).collect(),
            exists,
        }
    }

    fn only(files: &[&str]) -> impl Fn(&Path) -> bool {
        let set: Vec<String> = files.iter().map(|f| f.to_lowercase()).collect();
        move |p: &Path| set.contains(&p.to_string_lossy().to_lowercase())
    }

    /// The whole point of the exercise. WSL's `bash.exe` is first on `PATH` on
    /// a stock Windows box, so a search that takes the first hit takes it — and
    /// then the command runs in a filesystem namespace Emma's containment check
    /// has never heard of. Git for Windows is preferred *and* WSL is skipped
    /// rather than merely ranked below it, because on a box without Git the
    /// ranking alone would still fall through to WSL.
    #[test]
    fn windows_prefers_git_bash_and_never_falls_through_to_wsl() {
        let files = only(&[
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Windows\System32\bash.exe",
        ]);
        let e = env(
            true,
            &[r"C:\Program Files\Git\bin"],
            &[r"C:\Windows\System32"],
            &files,
        );
        let shell = resolve_with(None, &e).expect("git bash");
        assert_eq!(
            shell.path,
            PathBuf::from(r"C:\Program Files\Git\bin\bash.exe")
        );
        assert_eq!(shell.kind, ShellKind::Posix);

        // Git gone, WSL still there: a refusal, not WSL.
        let only_wsl = only(&[r"C:\Windows\System32\bash.exe"]);
        let e = env(true, &[], &[r"C:\Windows\System32"], &only_wsl);
        let err = resolve_with(None, &e).expect_err("wsl must not be chosen");
        assert_eq!(err.kind(), "tool_unavailable");
        assert!(err.detail().contains(OVERRIDE_ENV), "{err}");
    }

    /// The refusal has to name what was looked for, or the user is left
    /// reverse-engineering the search — the failure this change exists to end.
    #[test]
    fn the_refusal_names_what_it_looked_for() {
        let none = only(&[]);
        let e = env(true, &[r"C:\Program Files\Git\bin"], &[r"C:\bin"], &none);
        let err = resolve_with(None, &e).expect_err("nothing exists");
        let d = err.detail();
        assert!(d.contains("bash"), "{d}");
        assert!(d.contains(r"C:\Program Files\Git\bin"), "{d}");
        assert!(d.contains(OVERRIDE_ENV), "{d}");
    }

    #[test]
    fn unix_takes_bin_sh_before_anything_on_path() {
        let files = only(&["/bin/sh", "/opt/weird/sh"]);
        let e = env(false, &[], &["/opt/weird"], &files);
        let shell = resolve_with(None, &e).expect("sh");
        assert_eq!(shell.path, PathBuf::from("/bin/sh"));
        assert_eq!(shell.source, ShellSource::Default);
    }

    /// An absolute path in the override is taken as given. This is the escape
    /// hatch for every shell nobody thought to name.
    #[test]
    fn an_override_naming_a_path_wins() {
        let files = only(&["/bin/sh", "/usr/local/bin/dash"]);
        let e = env(false, &[], &["/bin"], &files);
        let shell = resolve_with(Some("/usr/local/bin/dash"), &e).expect("dash");
        assert_eq!(shell.path, PathBuf::from("/usr/local/bin/dash"));
        assert_eq!(shell.source, ShellSource::Override);
    }

    /// The refusal that matters most: a tool that silently runs a different
    /// shell than the one you named is worse than one that stops.
    #[test]
    fn an_override_naming_a_missing_shell_refuses_rather_than_substituting() {
        let files = only(&["/bin/sh"]);
        let e = env(false, &[], &["/bin"], &files);
        let err = resolve_with(Some("/opt/fish"), &e).expect_err("must not fall back");
        assert_eq!(err.kind(), "tool_unavailable");
        assert!(err.detail().contains("/opt/fish"), "{err}");
        assert!(err.detail().contains(OVERRIDE_ENV), "{err}");
    }

    /// PowerShell is reachable, but only because somebody asked for it by name.
    #[test]
    fn powershell_is_opt_in_and_never_a_default() {
        let files = only(&[
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Program Files\Git\bin\bash.exe",
        ]);
        let dirs = [
            r"C:\Windows\System32\WindowsPowerShell\v1.0",
            r"C:\Program Files\Git\bin",
        ];
        let e = env(true, &[], &dirs, &files);
        assert_eq!(
            resolve_with(None, &e).expect("default").kind,
            ShellKind::Posix,
            "PowerShell must never be picked by a search"
        );
        let shell = resolve_with(Some("powershell"), &e).expect("opt in");
        assert_eq!(shell.kind, ShellKind::PowerShell);
        assert_eq!(shell.source, ShellSource::Override);
        // `-c` under PowerShell is `-Command`'s abbreviation on pwsh and
        // nothing at all on Windows PowerShell 5.1. Getting this wrong turns
        // every call into a usage message.
        assert!(shell.args().contains(&"-Command"), "{:?}", shell.args());
        assert!(shell.args().contains(&"-NoProfile"), "{:?}", shell.args());
    }

    /// Asking for WSL by name is refused, and the refusal explains why rather
    /// than just saying no. See the module doc: the containment check and the
    /// shell would be talking about different filesystems, and when WSL cannot
    /// translate the directory it was handed it starts in the user's home
    /// **with exit 0** — a command running outside the approved root, silently.
    #[test]
    fn wsl_is_refused_even_when_asked_for_by_name() {
        let files = only(&[r"C:\Windows\System32\bash.exe"]);
        let e = env(true, &[], &[r"C:\Windows\System32"], &files);
        for spec in ["wsl", r"C:\Windows\System32\bash.exe"] {
            let err = resolve_with(Some(spec), &e).expect_err("wsl is refused");
            assert_eq!(err.kind(), "tool_unavailable", "{spec}");
            assert!(err.detail().to_lowercase().contains("wsl"), "{spec}: {err}");
            assert!(
                err.detail().contains("containment") || err.detail().contains("namespace"),
                "the refusal must say why: {err}"
            );
        }
    }

    #[test]
    fn the_wsl_launcher_is_recognised_wherever_windows_keeps_it() {
        for wsl in [
            r"C:\Windows\System32\bash.exe",
            r"C:\Windows\Sysnative\bash.exe",
            r"C:\Users\a\AppData\Local\Microsoft\WindowsApps\bash.exe",
            r"C:\Windows\System32\wsl.exe",
        ] {
            assert_eq!(classify(Path::new(wsl)), ShellKind::Wsl, "{wsl}");
        }
        for posix in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\msys64\usr\bin\sh.exe",
            "/bin/sh",
            "/usr/bin/bash",
        ] {
            assert_eq!(classify(Path::new(posix)), ShellKind::Posix, "{posix}");
        }
        for ps in [
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Program Files\PowerShell\7\pwsh.exe",
        ] {
            assert_eq!(classify(Path::new(ps)), ShellKind::PowerShell, "{ps}");
        }
    }

    /// An empty or whitespace override is treated as unset rather than as a
    /// shell named "". `EMMA_SHELL=` in a shell profile is a common way to
    /// *clear* a variable, and refusing it would be a confusing dead end.
    #[test]
    fn a_blank_override_means_unset() {
        let files = only(&["/bin/sh"]);
        let e = env(false, &[], &[], &files);
        assert_eq!(
            resolve_with(Some("   "), &e).expect("blank is unset").path,
            PathBuf::from("/bin/sh")
        );
    }

    /// The other half of that answer, stated as a rule the next edit has to
    /// pass. Everything the model is shown *before* the call is hashed into
    /// `tool_schema_hash`, and that hash is how anyone tells which tool surface
    /// produced a given answer. The moment a shell path or a per-platform
    /// sentence gets formatted into the description or the schema, the hash
    /// varies by machine and the attribution stops meaning anything — a
    /// regression nothing else here would catch, because the tool would work
    /// perfectly on every box and simply disagree with all the others.
    #[test]
    fn nothing_machine_specific_reaches_the_hashed_surface() {
        use emma_tool_api::Tool;
        let bash = Bash::new();
        let surface = format!("{}\n{}", bash.description(), bash.input_schema());
        for token in [
            "/bin/sh",
            "bash.exe",
            "Program Files",
            "System32",
            "WSL",
            "wsl",
            "sh -c",
            OVERRIDE_ENV,
        ] {
            assert!(
                !surface.contains(token),
                "{token:?} is machine- or shell-specific and must not be in the hashed surface"
            );
        }
        // And the positive half: it has to actually tell the model where the
        // answer is, or removing the claim was just removing information.
        assert!(surface.contains("first line"), "{surface}");
    }

    /// The banner is the whole answer to "the description cannot say which
    /// shell this is without the schema hash becoming machine-dependent": the
    /// shell is named in the result instead, at run time.
    #[test]
    fn the_banner_names_the_kind_and_the_path() {
        let shell = Shell {
            path: PathBuf::from("/bin/sh"),
            kind: ShellKind::Posix,
            source: ShellSource::Default,
        };
        assert!(shell.banner().contains("posix"));
        assert!(shell.banner().contains("/bin/sh"));
        assert_eq!(shell.banner().lines().count(), 1);
    }
}

// endregion: Resolution tests

// region: Background tests
// ---------------------------------------------------------------------------
// Background tests
//
// These spawn real children through the real shell, because the thing under
// test is lifetime: a call that returns while the child runs, output that
// arrives after the call is gone, a state carrying the exit nobody waited for
// in the call. A scripted child would prove none of that. They live here
// rather than in `tests/` so the file that owns the background path also owns
// the proof it works.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod background_tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    fn ctx_in(dir: &Path) -> ToolCtx {
        ToolCtx {
            cwd: dir.to_path_buf(),
            session_id: "bg-test".into(),
            turn_id: "t".into(),
            background: Default::default(),
        }
    }

    async fn call(ctx: &ToolCtx, args: serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Bash::new().invoke(ctx, args).await.expect("no fault")
    }

    /// Poll until the task reports a finished state, accumulating output.
    /// Bounded so a background path that never sets a final state fails as a
    /// named timeout rather than hanging the suite.
    async fn read_to_end(task: &Task) -> (TaskState, String) {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut seen = String::new();
        loop {
            let read = task.read_new();
            seen.push_str(&read.new_output);
            if read.state.finished() {
                return (read.state, seen);
            }
            assert!(
                Instant::now() < deadline,
                "the task never reached a final state; saw so far: {seen:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// The core claim: the call returns while the child is still running. The
    /// elapsed bound is what makes it a claim at all — `sleep 5` under the
    /// foreground path returns in five seconds and would pass every other
    /// assertion here. The `Running` state right after the call is the second
    /// witness, and the missing exit code is the third: an outcome carrying a
    /// code is an outcome a model may read as a finished command.
    #[tokio::test]
    async fn a_background_command_returns_before_the_command_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        let started = Instant::now();
        let outcome = call(
            &ctx,
            json!({ "command": "sleep 5", "run_in_background": true }),
        )
        .await
        .expect("background spawn");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the call waited for the child: {:?}",
            started.elapsed()
        );
        assert!(outcome.content.contains("bash_1"), "{outcome:?}");
        assert!(
            outcome.content.contains("has not finished"),
            "the outcome must be unreadable as a completion: {outcome:?}"
        );
        assert_eq!(
            outcome.exit_code, None,
            "an exit code on a background spawn claims a wait that never happened"
        );

        let task = ctx
            .background
            .get(&ctx.session_id, "bash_1")
            .expect("the task was not registered");
        assert_eq!(
            task.state(),
            TaskState::Running,
            "already finished — the call must have waited after all"
        );
        // `on_kill` was wired at spawn, or this returns false — and the sleep
        // is stopped rather than left to hold the pipes at runtime shutdown.
        assert!(task.kill(), "no killer was registered at spawn");
    }

    /// Output produced after the call returned still lands in the registry,
    /// and the final state is the real exit. The stderr assertion is not
    /// decoration: a background path that pumped only stdout would pass every
    /// other line of this test and silently lose every compiler diagnostic.
    #[tokio::test]
    async fn background_output_reaches_the_registry_with_the_real_exit() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        call(
            &ctx,
            json!({
                "command": "echo bg_marker_out; echo bg_marker_err >&2",
                "run_in_background": true
            }),
        )
        .await
        .expect("background spawn");

        let task = ctx.background.get(&ctx.session_id, "bash_1").expect("task");
        let (state, seen) = read_to_end(&task).await;
        assert!(seen.contains("bg_marker_out"), "stdout lost: {seen:?}");
        assert!(seen.contains("bg_marker_err"), "stderr lost: {seen:?}");
        assert_eq!(state, TaskState::Exited(Some(0)));
    }

    /// Non-zero specifically, because `Exited(Some(0))` is what a hardcoded
    /// success would report and a model acting on a background build needs
    /// the number, not a rounding of it.
    #[tokio::test]
    async fn a_background_exit_code_is_the_childs_not_a_default() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        call(&ctx, json!({ "command": "exit 7", "run_in_background": true }))
            .await
            .expect("background spawn");
        let task = ctx.background.get(&ctx.session_id, "bash_1").expect("task");
        let (state, _) = read_to_end(&task).await;
        assert_eq!(state, TaskState::Exited(Some(7)));
    }

    /// The child survives `invoke` returning — the guarantee `kill_on_drop`
    /// would silently destroy. The proof is work completed *after* the call
    /// was over: the marker file can only exist if the child outlived the
    /// return, so a path that reaps its child on return fails here and
    /// nowhere else.
    #[tokio::test]
    async fn the_child_outlives_the_call_that_spawned_it() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        let started = Instant::now();
        call(
            &ctx,
            json!({
                "command": "sleep 1 && echo done > marker.txt",
                "run_in_background": true
            }),
        )
        .await
        .expect("background spawn");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the call must return before the sleep ends for this to prove anything"
        );

        let marker = dir.path().join("marker.txt");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "the marker never appeared — the child did not survive the call returning"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let content = std::fs::read_to_string(&marker).expect("marker readable");
        assert!(content.contains("done"), "{content:?}");
    }

    /// The refusal that keeps `timeout_ms` honest: a background task has no
    /// wait to bound, and accepting-then-ignoring the argument would tell the
    /// caller a fuse is lit when nothing is armed. `bad_arguments`, not
    /// `tool_failed` — the caller mis-spoke and can retry in one move.
    #[tokio::test]
    async fn a_timeout_on_a_background_task_is_refused_not_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        let error = call(
            &ctx,
            json!({ "command": "echo hi", "run_in_background": true, "timeout_ms": 5000 }),
        )
        .await
        .expect_err("the pair must be refused");
        assert_eq!(error.kind(), "bad_arguments");
        assert!(error.detail().contains("timeout_ms"), "{error}");
        assert!(
            ctx.background.list(&ctx.session_id).is_empty(),
            "a refused call must not leave a task behind"
        );
    }
}

// endregion: Background tests
