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
//! **Exit status is not failure.** The ruling and the reasoning are recorded at
//! the point in `run` that implements it, below. One thing to know before
//! reading further: `descriptions/bash.md`, which is what the model is actually
//! shown, still describes a non-zero exit as a failure. That text predates the
//! ruling and now contradicts the code. It is left alone here because the
//! description is part of the tool's wire surface and changing it changes the
//! registry's schema hash, so it is a deliberate edit rather than a drive-by
//! one — but it is wrong, and it is wrong in the direction that teaches the
//! model to avoid `Bash` for exactly the commands it should be using it for.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

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
const KEYS: &[&str] = &["command", "timeout_ms", "cwd"];

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
                "command": { "type": "string", "description": "Shell command line, run with sh -c." },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!("Default {DEFAULT_TIMEOUT_MS}, capped at {MAX_TIMEOUT_MS}.")
                },
                "cwd": {
                    "type": "string",
                    "description": "Directory to start in. Must be inside the working directory. Defaults to it."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: false,
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

        let shell = find_shell()?;
        let mut cmd = tokio::process::Command::new(&shell);
        cmd.arg("-c")
            .arg(command)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Without this a timed-out command keeps running after Emma has
            // given up on it, and the next call sees a machine still busy with
            // work nobody is waiting for.
            .kill_on_drop(true)
            .env_clear();
        for key in ENV_ALLOWLIST {
            if let Ok(value) = std::env::var(key) {
                cmd.env(key, value);
            }
        }

        let mut child = cmd.spawn().map_err(|e| {
            ToolError::Unavailable(format!("{} could not be started: {e}", shell.display()))
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
        let ((out, out_cut), (err, err_cut)) =
            match tokio::time::timeout(Duration::from_secs(5), drains).await {
                Ok(Ok(pair)) => pair,
                _ => ((Vec::new(), true), (Vec::new(), true)),
            };

        let body = render(&out, &err);
        let cut = out_cut || err_cut;

        // A timeout is one of the three genuine failures — the command did not
        // finish, so there is no status to report and nothing answered. The
        // output produced before the kill still goes in the message: a build
        // that hung after printing where it hung is telling you where it hung.
        if timed_out {
            return Err(ToolError::Failed(format!(
                "the command was killed after {timeout_ms}ms.\n{body}"
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
        if cut {
            content.push_str(&format!(
                "\n[truncated: output cut at {MAX_STREAM_BYTES} bytes per stream]"
            ));
        }
        let outcome = ToolOutcome::new(content);
        Ok(if cut { outcome.truncated() } else { outcome })
    }
}

// endregion: Running the command

// region: Output, and finding a shell
// ---------------------------------------------------------------------------
// Output, and finding a shell
//
// How the two streams are turned into one readable body, how they are read
// without deadlocking the child, and the refusal that happens when there is no
// POSIX shell to run anything with.
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

/// A POSIX shell, or an honest refusal.
///
/// On Windows this looks for `bash` on `PATH` — Git for Windows or WSL — and
/// deliberately does not fall back to `cmd.exe`. Silently running a command
/// written for `sh` under a shell with different quoting, different globbing
/// and different operators would produce wrong results that look like the
/// command's own output. `Unavailable` says "I cannot do this", which is
/// exactly what the variant is for.
pub fn find_shell() -> Result<PathBuf, ToolError> {
    if cfg!(unix) {
        let sh = PathBuf::from("/bin/sh");
        if sh.exists() {
            return Ok(sh);
        }
    }
    let exe = if cfg!(windows) { "bash.exe" } else { "sh" };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(ToolError::Unavailable(
        "no POSIX shell is available; Bash needs sh (or bash on Windows) on PATH".into(),
    ))
}

// endregion: Output, and finding a shell
