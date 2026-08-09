//! Hooks: operator-authored programs the runtime runs at two dispatch sites.
//!
//! **Why this is not in `lib.rs`.** Everything else in the harness — personas,
//! skills, commands, the spine — really is "file reads, one serde struct". This
//! is not. A hook runs a program at the highest-privilege point of the turn, so
//! it canonicalises and containment-checks the path, refuses a non-executable
//! file, clears the environment down to an allowlist, hashes the executable it
//! is about to run, anchors the matcher, bounds the wall clock, caps both pipes,
//! and decides what a malformed answer means. That is a subprocess supervisor.
//!
//! So the split is not bookkeeping: the two files fail for different reasons.
//! `lib.rs` growing means configuration is sprouting behaviour. This file
//! growing means the supervisor is doing more to a hook than run it and read its
//! answer.
//!
//! **The one policy that lives here, because it is about hooks and not about the
//! loop:** `PreToolUse` is fail-closed and `PostToolUse` is fail-open. See `run`
//! — it is four lines and it is the whole design.
//!
//! Nothing here writes to a log or knows an event type. The caller owns any
//! record it wants to keep, which is what lets every test in this crate run
//! without a process around it.
//!
//! **Emma's stake is higher than tustle-agent's was.** There the tool surface
//! was read-only by construction, so a `PreToolUse` hook guarded a search. Here
//! it guards `Bash` and `Write`. Every check below was already justified; none
//! of them is now optional.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::hash;

const DEFAULT_HOOK_TIMEOUT_MS: u64 = 5_000;
/// Hook time is spent inside the user's own patience, so the engine caps what
/// config may ask for. A hook is a gate, not a job runner. Config asking for
/// more is silently reduced rather than refused: the operator asked for a
/// longer gate, not for a different program.
const MAX_HOOK_TIMEOUT_MS: u64 = 10_000;
const HOOK_OUTPUT_CAP: u64 = 64 * 1024;
const HOOK_REASON_CAP: usize = 400;

/// The environment a hook is given, and all of it. `SYSTEMROOT` and `COMSPEC`
/// are Windows process-creation requirements, not policy.
///
/// This process holds `ANTHROPIC_API_KEY`. A hook is operator-authored, but it
/// is still a separate program running at the highest-privilege point of the
/// turn: it gets what it needs to execute and nothing that would let it call a
/// model or a paid API as us. A hook that needs a value reads it from a file
/// next to itself.
const HOOK_ENV_ALLOWLIST: &[&str] = &["PATH", "HOME", "LANG", "TMPDIR", "SYSTEMROOT", "COMSPEC"];

// ---------------------------------------------------------------------------
// Declaration — the `hooks` block of `.emma/config.json`
// ---------------------------------------------------------------------------

/// `deny_unknown_fields`, like everything in Emma's own spine: a misspelled
/// `mathcer` that quietly matched every tool is the failure this costs one
/// attribute to prevent.
///
/// `event` is a `String` rather than the enum so that an unimplemented event
/// name produces our sentence rather than serde's. See `HookEvent::parse`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookDef {
    pub(crate) event: String,
    /// Relative to the root, and must canonicalise to inside `<root>/hooks/`.
    pub(crate) command: String,
    #[serde(default)]
    pub(crate) matcher: Option<String>,
    #[serde(default)]
    pub(crate) timeout_ms: Option<u64>,
    #[serde(default)]
    pub(crate) text: Option<String>,
}

/// A closed set with two members. Config attaches commands to dispatch sites; it
/// can never mint a third.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
}

impl HookEvent {
    pub const ALL: &'static [&'static str] = &["PreToolUse", "PostToolUse"];

    /// Claude Code implements a larger set — `SessionStart`, `UserPromptSubmit`,
    /// `Stop` and others. Emma implements two, and a config naming one of the
    /// rest is a **startup error, never a silent skip**: a security hook that
    /// quietly never runs is worse than no hook, because the operator believes
    /// they have one. See `notes/claude-code-compatibility.md`.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "PreToolUse" => Ok(Self::PreToolUse),
            "PostToolUse" => Ok(Self::PostToolUse),
            other => bail!(
                "hook event `{other}` is not implemented by Emma (implemented: {}). \
                 A hook attached to an event that never fires is a policy the \
                 operator believes they have; remove it or implement the event",
                Self::ALL.join(", ")
            ),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ResolvedHook {
    name: String,
    event: HookEvent,
    command: PathBuf,
    command_hash: String,
    matcher: Option<regex::Regex>,
    timeout: Duration,
    text: Option<String>,
}

impl ResolvedHook {
    /// For `Harness::snapshot`: identity of what will run, never its text.
    pub(crate) fn identity(&self) -> serde_json::Value {
        serde_json::json!({ "name": self.name, "event": self.event, "hash": self.command_hash })
    }
}

// ---------------------------------------------------------------------------
// The call, and what a hook may answer
// ---------------------------------------------------------------------------

/// Everything a hook is told about a call.
///
/// Deliberately **not** built from a `&ToolOutcome`: `display` is for the
/// terminal and is not what the model saw, and a hook shown more than the model
/// is shown would be a side channel the operator did not know they were
/// operating. Plumbing another internal field through to an external process
/// needs a signature change here, which a reviewer will see.
pub struct HookCall<'a> {
    pub tool_name: &'a str,
    pub tool_call_id: &'a str,
    pub args: &'a serde_json::Value,
    pub session_id: &'a str,
    pub turn_id: &'a str,
    /// `PostToolUse` only. Absent on the pre-flight, where there is no result.
    pub result: Option<HookResult<'a>>,
}

/// The model-visible outcome of a call, and nothing more.
pub struct HookResult<'a> {
    /// Exactly the text that rode the `tool_result` block.
    pub content: &'a str,
    pub truncated: bool,
    /// `Some` when the tool returned a `ToolError`. The model was told, so a
    /// hook may be told: an audit hook that cannot tell a success from a failure
    /// is an audit log that cannot answer the question it exists for.
    pub error: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookOutcome {
    Allow,
    Deny,
    /// Timed out, crashed, exited non-zero, or wrote garbage. Which side of the
    /// invoke it lands on decides what it means — see `run`.
    Failed,
}

/// One hook execution, shaped to drop straight into whatever the caller logs.
#[derive(Debug, Clone, Serialize)]
pub struct HookRun {
    pub hook: String,
    pub event: HookEvent,
    pub command_hash: String,
    pub outcome: HookOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub reason: Option<String>,
    pub context: Option<String>,
    /// Captured for the log. Never reaches the model.
    pub stderr: String,
}

#[derive(Debug, Default)]
pub struct HookVerdict {
    /// `Some(reason)` only ever from a `PreToolUse` hook. The tool must not run.
    pub denied: Option<String>,
    /// Additive text a hook asked to append to the model-visible result. A hook
    /// can annotate a result; it can never rewrite one.
    pub context: Vec<String>,
    pub runs: Vec<HookRun>,
}

impl HookVerdict {
    pub fn is_denied(&self) -> bool {
        self.denied.is_some()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HookReply {
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    context: Option<String>,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Run every hook attached to `event` that matches this call, in hook-name
/// order — the key in the `hooks` map, sorted. Not declaration order, which JSON
/// does not preserve anyway.
pub(crate) async fn run(
    hooks: &[ResolvedHook],
    event: HookEvent,
    call: &HookCall<'_>,
) -> HookVerdict {
    let mut verdict = HookVerdict::default();
    for hook in hooks
        .iter()
        .filter(|h| h.event == event && h.matches(call.tool_name))
    {
        let run = hook.invoke(call).await;
        let denied = run.outcome != HookOutcome::Allow;
        let reason = run.reason.clone();
        if let Some(ctx) = run.context.clone() {
            verdict.context.push(ctx);
        }
        verdict.runs.push(run);
        // Asymmetric on purpose, and this is the whole hook design in four lines.
        // A `PreToolUse` hook that denies — or that times out, crashes, or
        // answers with garbage — stops the call: ambiguity resolves to deny,
        // because a broken policy check must not degrade into no policy check. A
        // `PostToolUse` failure changes nothing: the tool ran, the side effect
        // happened, the result is already recorded, and pretending otherwise
        // would make the log lie about what the model saw.
        if denied && event == HookEvent::PreToolUse {
            verdict.denied =
                Some(reason.unwrap_or_else(|| "This call was blocked by policy.".into()));
            return verdict;
        }
    }
    verdict
}

impl ResolvedHook {
    fn matches(&self, tool_name: &str) -> bool {
        self.matcher.as_ref().is_none_or(|m| m.is_match(tool_name))
    }

    fn payload(&self, call: &HookCall<'_>) -> serde_json::Value {
        let mut v = serde_json::json!({
            "event": self.event,
            "tool_name": call.tool_name,
            "tool_call_id": call.tool_call_id,
            "args": call.args,
            "session_id": call.session_id,
            "turn_id": call.turn_id,
            "config_text": self.text,
        });
        if let Some(r) = &call.result {
            v["result_content"] = serde_json::Value::String(r.content.to_string());
            v["truncated"] = serde_json::Value::Bool(r.truncated);
            v["error"] = match r.error {
                Some(e) => serde_json::Value::String(e.to_string()),
                None => serde_json::Value::Null,
            };
        }
        v
    }

    async fn invoke(&self, call: &HookCall<'_>) -> HookRun {
        let started = Instant::now();
        let mut run = HookRun {
            hook: self.name.clone(),
            event: self.event,
            command_hash: self.command_hash.clone(),
            // Failed until proven otherwise: every early return below is a
            // failure, so the default must be the one that denies.
            outcome: HookOutcome::Failed,
            exit_code: None,
            duration_ms: 0,
            reason: None,
            context: None,
            stderr: String::new(),
        };

        // argv exec of a canonicalised path: no shell, no PATH lookup, no
        // argument string for a tool name to be interpolated into.
        let mut cmd = tokio::process::Command::new(&self.command);
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        cmd.env_clear();
        for key in HOOK_ENV_ALLOWLIST {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }

        let body = self.payload(call).to_string();
        let finished = tokio::time::timeout(self.timeout, exec(&mut cmd, body)).await;
        run.duration_ms = started.elapsed().as_millis() as u64;
        let (code, stdout, stderr) = match finished {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                run.stderr = format!("spawn failed: {e}");
                return run;
            }
            Err(_) => {
                run.stderr = format!("timed out after {}ms", self.timeout.as_millis());
                return run;
            }
        };
        run.stderr = String::from_utf8_lossy(&stderr).into_owned();
        run.exit_code = code;
        if code != Some(0) {
            return run;
        }

        let stdout = String::from_utf8_lossy(&stdout);
        let stdout = stdout.trim();
        if stdout.is_empty() {
            // The normal answer for an observer.
            run.outcome = HookOutcome::Allow;
            return run;
        }
        let Ok(reply) = serde_json::from_str::<HookReply>(stdout) else {
            run.stderr.push_str("\nunparseable stdout");
            return run;
        };
        run.context = reply.context;
        // A hook's own reason, else the operator's configured `text`, else the
        // generic line `run` supplies. Capped, and logged verbatim.
        run.reason = reply.reason.or_else(|| self.text.clone()).map(|mut r| {
            r.truncate(HOOK_REASON_CAP);
            r
        });
        run.outcome = match reply.decision.as_deref() {
            Some("deny") => HookOutcome::Deny,
            _ => HookOutcome::Allow,
        };
        run
    }
}

/// Spawn, feed stdin, read both pipes under a hard cap. A hook that writes past
/// the cap blocks and hits the timeout, which is the right answer for a program
/// that will not stop.
async fn exec(
    cmd: &mut tokio::process::Command,
    body: String,
) -> std::io::Result<(Option<i32>, Vec<u8>, Vec<u8>)> {
    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // A hook that ignores stdin and exits gives us EPIPE. That is its
        // choice, not a failure.
        let _ = stdin.write_all(body.as_bytes()).await;
    }
    let (mut out, mut err) = (Vec::new(), Vec::new());
    if let Some(so) = child.stdout.take() {
        so.take(HOOK_OUTPUT_CAP).read_to_end(&mut out).await?;
    }
    if let Some(se) = child.stderr.take() {
        se.take(HOOK_OUTPUT_CAP).read_to_end(&mut err).await?;
    }
    Ok((child.wait().await?.code(), out, err))
}

// ---------------------------------------------------------------------------
// Resolution — every check that can be made before a hook ever runs
// ---------------------------------------------------------------------------

pub(crate) fn resolve(
    root: &Path,
    spine_path: &Path,
    declared: &BTreeMap<String, HookDef>,
    enabled: Option<&[String]>,
) -> Result<Vec<ResolvedHook>> {
    let named = |what: String| format!("{}: {what}", spine_path.display());
    for name in enabled.unwrap_or_default() {
        if !declared.contains_key(name) {
            bail!(named(format!(
                "persona enables hook `{name}`, undefined here"
            )));
        }
    }
    let mut out = Vec::new();
    for (name, def) in declared {
        // A hook declared in the spine but excluded by the persona is never
        // resolved, so it costs nothing — including `"hooks": []`, which turns
        // all of them off.
        if enabled.is_some_and(|e| !e.contains(name)) {
            continue;
        }
        let event = HookEvent::parse(&def.event)
            .with_context(|| named(format!("hook `{name}`")))?;
        let dir = (root.join("hooks").canonicalize())
            .with_context(|| named(format!("hook `{name}` is defined but hooks/ is missing")))?;
        let command = (root.join(&def.command).canonicalize())
            .with_context(|| named(format!("hook `{name}`: no such command `{}`", def.command)))?;
        // The containment check is why `command` is relative: an absolute path
        // or a `..` escape would let the spine run any executable on the box
        // with Emma's permissions — and Emma's permissions include writing the
        // user's source tree.
        if !command.starts_with(&dir) {
            bail!(named(format!(
                "hook `{name}` resolves to {}, outside {}",
                command.display(),
                dir.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(&command)?.permissions().mode() & 0o111 == 0 {
                bail!(named(format!("hook `{name}` is not executable")));
            }
        }
        let matcher = match &def.matcher {
            // Anchored: a matcher is matched in full, so `Read` cannot silently
            // guard `ReadFile` — the near miss that looks like a working policy.
            Some(m) => Some(
                regex::Regex::new(&format!("^(?:{m})$"))
                    .with_context(|| named(format!("hook `{name}`: `{m}` is not a regex")))?,
            ),
            None => None,
        };
        out.push(ResolvedHook {
            name: name.clone(),
            event,
            command_hash: hash::short(&String::from_utf8_lossy(&std::fs::read(&command)?)),
            command,
            matcher,
            timeout: Duration::from_millis(
                (def.timeout_ms.unwrap_or(DEFAULT_HOOK_TIMEOUT_MS)).min(MAX_HOOK_TIMEOUT_MS),
            ),
            text: def.text.clone(),
        });
    }
    Ok(out)
}
