//! Hooks: operator-authored programs the runtime runs at three dispatch sites.
//!
//! **Why this is not in `lib.rs`.** Everything else in the harness — personas,
//! skills, commands, the spine — really is "file reads, one serde struct". This
//! is not. A hook runs a program at the highest-privilege point of the turn, so
//! it canonicalises and containment-checks the path, refuses a non-executable
//! file (on unix, where there is an executable bit to check), clears the
//! environment down to a six-name allowlist, hashes the executable it is about
//! to run, anchors the matcher, bounds the wall clock, caps both pipes, and
//! decides what a malformed answer means. That is a subprocess supervisor.
//!
//! Everything in that list except the timeout and the pipe caps happens in
//! `resolve`, at load. A hook that cannot be run safely stops the boot rather
//! than failing at the moment it was needed, which is the same ruling as every
//! other refusal in this crate: the honest answer to an ambiguous configuration
//! is not to start.
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
//! **The third site is not a tool call.** `UserPromptSubmit` fires once, on the
//! words a person typed, before the goal opens — see `run_prompt`. It is the one
//! event that can *add* to what the model reads rather than only refuse
//! something, so it has its own call type (`PromptCall`), its own verdict
//! (`PromptVerdict`), and a third failure ruling that is neither of the two
//! above: **fail-open, but never silently.** The argument is in `run_prompt`.
//!
//! Nothing here writes to a log or knows an event type. The caller owns any
//! record it wants to keep, which is what lets every test in this crate run
//! without a process around it.
//!
//! **Emma's stake is higher than the predecessor's was.** There the tool surface
//! was read-only by construction, so a `PreToolUse` hook guarded a search. Here
//! it guards `Bash` and `Write`. Every check below was already justified; none
//! of them is now optional.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::hash;

// region: The caps, and the environment a hook is given
// ---------------------------------------------------------------------------
// The caps, and the environment a hook is given
//
// The bounds the engine imposes no matter what config asks for, and the six
// variables a hook process inherits. These are the numbers a reviewer wants
// first, so they are the first thing in the file.
// ---------------------------------------------------------------------------

const DEFAULT_HOOK_TIMEOUT_MS: u64 = 5_000;
/// Hook time is spent inside the user's own patience, so the engine caps what
/// config may ask for. A hook is a gate, not a job runner. Config asking for
/// more is silently reduced rather than refused: the operator asked for a
/// longer gate, not for a different program.
const MAX_HOOK_TIMEOUT_MS: u64 = 10_000;
pub(crate) const HOOK_OUTPUT_CAP: u64 = 64 * 1024;
const HOOK_REASON_CAP: usize = 400;
/// How much injected context one hook may put in front of the model, in
/// characters. Claude Code's own limit, kept to the character so a script sized
/// against that documentation behaves the same here.
///
/// It is not the pipe cap doing this job. `HOOK_OUTPUT_CAP` stops a program
/// filling memory; this stops a program filling the *prompt*, which is a
/// different resource with a different owner — the user pays for it by the
/// token, on this turn and on every later turn that replays it.
///
/// Claude Code spills the overflow to a file and passes the path. Emma
/// truncates and says so in the text, because a path is only useful to a model
/// that can read the file, and the honest failure is the one the model can see.
const HOOK_CONTEXT_CAP: usize = 10_000;

/// The environment a hook is given, and all of it — six names. `SYSTEMROOT` and
/// `COMSPEC` are Windows process-creation requirements, not policy; the other
/// four are what a small program needs to find its interpreter and behave.
///
/// This process holds `ANTHROPIC_API_KEY`. A hook is operator-authored, but it
/// is still a separate program running at the highest-privilege point of the
/// turn: it gets what it needs to execute and nothing that would let it call a
/// model or a paid API as us. A hook that needs a value reads it from a file
/// next to itself.
pub(crate) const HOOK_ENV_ALLOWLIST: &[&str] =
    &["PATH", "HOME", "LANG", "TMPDIR", "SYSTEMROOT", "COMSPEC"];

// endregion: The caps, and the environment a hook is given

// region: Declaration — the hooks block of .emma/config.json
// ---------------------------------------------------------------------------
// Declaration — the `hooks` block of `.emma/config.json`
//
// What an operator is allowed to write, and the closed set of events they may
// attach it to. `HookDef` is the unvalidated form straight off the file;
// `ResolvedHook` further down is what survived every check.
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

/// A closed set with three members. Config attaches commands to dispatch sites;
/// it can never mint a fourth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    /// Once per prompt a person typed, before the goal opens. The only event
    /// that can add to what the model reads.
    UserPromptSubmit,
}

impl HookEvent {
    pub const ALL: &'static [&'static str] = &["PreToolUse", "PostToolUse", "UserPromptSubmit"];

    /// Claude Code implements a larger set — `SessionStart`, `Stop`,
    /// `PreCompact` and others. Emma implements three, and a config naming one
    /// of the rest is a **startup error, never a silent skip**: a security hook
    /// that quietly never runs is worse than no hook, because the operator
    /// believes they have one. See `notes/design/claude-code-compatibility.md`.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "PreToolUse" => Ok(Self::PreToolUse),
            "PostToolUse" => Ok(Self::PostToolUse),
            "UserPromptSubmit" => Ok(Self::UserPromptSubmit),
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

// endregion: Declaration — the hooks block of .emma/config.json

// region: The call, and what a hook may answer
// ---------------------------------------------------------------------------
// The call, and what a hook may answer
//
// Both directions of the contract with an external process: exactly what a hook
// is told, and exactly what it is allowed to say back. Every type here is a
// boundary — widening one hands an operator-authored program more of Emma's
// insides, which is why they are declared together rather than beside their
// users.
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

/// Everything a `UserPromptSubmit` hook is told: the words a person typed, and
/// where they typed them.
///
/// Separate from [`HookCall`] rather than a variant of it, because the two
/// payloads have no field in common beyond the session and every field one of
/// them grows is a field the other would have to answer `null` for. A tool hook
/// that could read the prompt would also be a side channel nobody configured.
pub struct PromptCall<'a> {
    /// Exactly what the user typed, before anything is prepended to it.
    pub prompt: &'a str,
    pub session_id: &'a str,
    /// The session JSONL. Claude Code's scripts read the transcript to see what
    /// has happened so far, so the path is sent under Claude Code's name for it.
    pub transcript_path: &'a str,
    pub cwd: &'a str,
}

/// What the prompt hooks decided, together.
///
/// `blocked` and `context` are independent: a hook that blocks contributes no
/// context (there is no turn to enrich), and a hook that enriches never blocks.
#[derive(Debug, Default)]
pub struct PromptVerdict {
    /// `Some(reason)` when a hook stopped the prompt. The goal must not open,
    /// and the reason is the user's to read.
    pub blocked: Option<String>,
    /// Text hooks asked to put in front of the model, in hook-name order.
    pub context: Vec<String>,
    pub runs: Vec<HookRun>,
}

impl PromptVerdict {
    pub fn is_blocked(&self) -> bool {
        self.blocked.is_some()
    }

    /// One sentence per hook that did not answer, for the terminal.
    ///
    /// **This exists because the failure here is fail-open.** A turn that
    /// quietly lost its enrichment looks exactly like a turn that was never
    /// configured to have any, and the operator would go on believing the branch
    /// name was in the prompt. Silence is the bug; this is the fix.
    pub fn notices(&self) -> Vec<String> {
        let mut out = Vec::new();
        for r in &self.runs {
            if r.outcome == HookOutcome::Failed {
                let why = r.stderr.lines().next().unwrap_or("").trim();
                out.push(match (r.exit_code, why.is_empty()) {
                    (_, false) => format!("hook `{}` added no context: {why}", r.hook),
                    (Some(c), true) => format!("hook `{}` added no context: exited {c}", r.hook),
                    (None, true) => format!("hook `{}` added no context", r.hook),
                });
            }
            // `systemMessage` is the hook talking to the person, and it is said
            // whether or not the hook also failed.
            if let Some(m) = &r.message {
                out.push(m.clone());
            }
        }
        out
    }
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
    /// `systemMessage`: a word for the *user*, never for the model. Kept
    /// separate from `context` for exactly that reason — one of these two is
    /// paid for in tokens and read by a model, and mixing them would make which
    /// is which a matter of where the caller happened to print it.
    pub message: Option<String>,
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

/// What a hook may print on stdout, in both spellings.
///
/// **Why this grew Claude Code's spelling rather than keeping Emma's.** The
/// whole reason a `.claude/settings.json` is read at all is that the scripts it
/// names keep working, and a real `UserPromptSubmit` hook does not print
/// `{"context": …}` — it prints
/// `{"hookSpecificOutput": {"hookEventName": …, "additionalContext": …}}`,
/// which is what Claude Code's own documentation tells people to write. Before
/// this, that object was an unknown field: unparseable stdout, which on
/// `PreToolUse` means *deny*. A compatibility feature that turns a working
/// upstream hook into a wall is not a compatibility feature.
///
/// **Still `deny_unknown_fields`, on both levels.** A misspelled `raeson` that
/// silently allowed is the failure the attribute exists to prevent, and it is
/// worth more than tolerance for a key Claude Code has not shipped yet. The two
/// events resolve an unreadable answer differently and both resolutions are
/// safe: `PreToolUse` denies (unchanged), `UserPromptSubmit` proceeds without
/// the context and says so.
///
/// The universal fields are declared so that a hook printing them is not
/// rejected; each one's comment says whether Emma acts on it, because a field
/// accepted and quietly ignored is the same lie as an event that never fires.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct HookReply {
    /// `deny` is Emma's spelling, `block` is Claude Code's on this event. Both
    /// mean the same thing and both are honoured.
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    /// Emma's spelling of additive context.
    #[serde(default)]
    context: Option<String>,
    #[serde(default, rename = "hookSpecificOutput")]
    specific: Option<HookSpecificOutput>,
    /// Honoured: `false` stops the prompt or the call, with `stopReason` as the
    /// sentence. Claude Code documents it as taking precedence over the
    /// event-specific decision, and it does here too.
    #[serde(default, rename = "continue")]
    keep_going: Option<bool>,
    #[serde(default, rename = "stopReason")]
    stop_reason: Option<String>,
    /// Accepted, and shown to the user by the caller when there is one. Emma has
    /// no separate warning channel, so it rides the same notice list.
    #[serde(default, rename = "systemMessage")]
    system_message: Option<String>,
    /// Accepted and inert. It hides a hook's stdout in Claude Code's transcript;
    /// Emma never prints hook stdout in the first place.
    #[serde(default, rename = "suppressOutput")]
    _suppress_output: Option<bool>,
    /// Accepted and inert: Emma's sessions are named by id, not by title.
    #[serde(default, rename = "sessionTitle")]
    _session_title: Option<String>,
    /// Accepted and inert. Emma always shows the user their own blocked prompt —
    /// see `run_prompt`, where the trace is the point.
    #[serde(default, rename = "suppressOriginalPrompt")]
    _suppress_original_prompt: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct HookSpecificOutput {
    /// Required by Claude Code, read by nothing here: the runtime already knows
    /// which event it dispatched, and trusting a child process's claim about
    /// that would let a hook answer for an event it was not attached to.
    #[serde(default, rename = "hookEventName")]
    _event: Option<String>,
    #[serde(default, rename = "additionalContext")]
    additional_context: Option<String>,
    /// Claude Code's `PreToolUse` spelling of a verdict. Honoured, because the
    /// alternative is reading a file that says `deny` and running the tool.
    /// `ask` maps to deny: Emma's approval gate is asked separately and a hook
    /// that wanted a human is not asking for the call to proceed unattended.
    #[serde(default, rename = "permissionDecision")]
    permission_decision: Option<String>,
    #[serde(default, rename = "permissionDecisionReason")]
    permission_reason: Option<String>,
    #[serde(default, rename = "sessionTitle")]
    _session_title: Option<String>,
}

// endregion: The call, and what a hook may answer

// region: Dispatch
// ---------------------------------------------------------------------------
// Dispatch
//
// Running the thing: the fail-closed/fail-open split in `run`, then the
// supervision of one child process in `invoke` and `exec`. This is the part
// that executes at the highest-privilege point of the turn.
// ---------------------------------------------------------------------------

/// Run every hook attached to `event` that matches this call, in hook-name
/// order — the key in the `hooks` map, sorted. Not declaration order, which JSON
/// does not preserve anyway.
///
/// Tool events only. `UserPromptSubmit` is excluded here as well as filtered by
/// `event`, so a caller that passes it by mistake dispatches nothing rather than
/// handing a prompt hook a payload with a tool name in it and no prompt.
pub(crate) async fn run(
    hooks: &[ResolvedHook],
    event: HookEvent,
    call: &HookCall<'_>,
) -> HookVerdict {
    let mut verdict = HookVerdict::default();
    for hook in hooks.iter().filter(|h| {
        h.event == event && h.event != HookEvent::UserPromptSubmit && h.matches(call.tool_name)
    }) {
        let run = hook.invoke(hook.payload(call).to_string()).await;
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

/// Run every `UserPromptSubmit` hook, in hook-name order, over the words a
/// person just typed.
///
/// **The third failure ruling, and why it is neither of the other two.**
/// `PreToolUse` denies on ambiguity because a broken gate must not become no
/// gate. `PostToolUse` allows on ambiguity because the side effect already
/// happened. Here a hook that crashes has failed at *enrichment*, and resolving
/// that to deny would mean a hook script with a syntax error locks the user out
/// of their own agent — every prompt refused, by the program that was supposed
/// to add the branch name to it. So: **the prompt proceeds, without the
/// context, and the caller is handed a sentence saying so** ([`PromptVerdict::notices`]).
/// Losing enrichment silently is the failure this is shaped to avoid; losing it
/// loudly is a bad turn the user can see and fix.
///
/// This also happens to be Claude Code's documented behaviour on this event — a
/// hook that times out is cancelled, its output including `additionalContext` is
/// discarded, and the prompt still reaches the model — so an existing
/// configuration behaves the same under both runtimes.
///
/// **Blocking is still real**, because a `UserPromptSubmit` hook that says
/// `block` is not failing, it is answering. Two spellings, both from Claude
/// Code: a JSON decision, and exit code 2 with the reason on stderr. A block
/// short-circuits the rest, exactly as a `PreToolUse` denial does.
pub(crate) async fn run_prompt(hooks: &[ResolvedHook], call: &PromptCall<'_>) -> PromptVerdict {
    let mut verdict = PromptVerdict::default();
    for hook in hooks
        .iter()
        .filter(|h| h.event == HookEvent::UserPromptSubmit)
    {
        let mut run = hook.invoke(hook.prompt_payload(call).to_string()).await;
        // Exit 2 is Claude Code's "block" in the exit-code channel, and its
        // message is stderr when the hook printed no reason. Every *other*
        // non-zero exit is a crash, which is the fail-open path below.
        let blocked_by_exit = run.exit_code == Some(2);
        if blocked_by_exit {
            run.outcome = HookOutcome::Deny;
            if run.reason.is_none() {
                let first = run.stderr.lines().next().unwrap_or("").trim();
                if !first.is_empty() {
                    run.reason = Some(first.to_string());
                }
            }
        }
        let outcome = run.outcome;
        let reason = run.reason.clone();
        if let Some(mut ctx) = run.context.clone() {
            if outcome == HookOutcome::Allow {
                if ctx.len() > HOOK_CONTEXT_CAP {
                    truncate_on_a_char_boundary(&mut ctx, HOOK_CONTEXT_CAP);
                    // Named in the text the model reads, because a model shown a
                    // sentence that stops mid-clause will otherwise reason about
                    // the half it was given as though it were the whole.
                    ctx.push_str(&format!(
                        "\n[hook `{}` printed more than {HOOK_CONTEXT_CAP} characters of \
                         context; the rest was cut]",
                        hook.name
                    ));
                }
                verdict.context.push(ctx);
            }
        }
        verdict.runs.push(run);
        if outcome == HookOutcome::Deny {
            verdict.blocked =
                Some(reason.unwrap_or_else(|| "This prompt was blocked by policy.".into()));
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

    /// What a `UserPromptSubmit` hook is told.
    ///
    /// **Two names for the event, and that is deliberate.** `event` is Emma's,
    /// so a hook written against `PreToolUse` here reads the same key on every
    /// event; `hook_event_name` is Claude Code's, so a script copied out of
    /// somebody's `.claude/hooks/` finds the field its examples use. The rest of
    /// the names are Claude Code's outright, because the scripts that exist
    /// already read `prompt`, `session_id`, `transcript_path` and `cwd`, and a
    /// better name would only be better for a file nobody has written yet.
    ///
    /// `permission_mode` is not sent. Emma's approval gate is not Claude Code's
    /// mode enum, and a plausible-looking `"default"` would be a fact a hook
    /// could branch on and be wrong about.
    fn prompt_payload(&self, call: &PromptCall<'_>) -> serde_json::Value {
        serde_json::json!({
            "event": self.event,
            "hook_event_name": "UserPromptSubmit",
            "prompt": call.prompt,
            "session_id": call.session_id,
            "transcript_path": call.transcript_path,
            "cwd": call.cwd,
            "config_text": self.text,
        })
    }

    async fn invoke(&self, body: String) -> HookRun {
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
            message: None,
            stderr: String::new(),
        };

        let mut cmd = contained_command(&self.command);
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
            // On `UserPromptSubmit`, plain stdout *is* the context — that is the
            // documented primary form, and the JSON object is the elaborate
            // one. Requiring JSON here made a hook that does the ordinary thing
            // (`echo` a line of context, exit 0) record `unparseable stdout` and
            // throw the line away, which is the whole feature failing in the
            // shape people actually write it. Found by running one.
            //
            // The other events keep the old resolution: they have no use for
            // free text, so unreadable stdout there is a hook that meant
            // something it failed to say, and `PreToolUse` denies on it.
            if self.event == HookEvent::UserPromptSubmit {
                run.context = Some(stdout.to_string());
                run.outcome = HookOutcome::Allow;
                return run;
            }
            run.stderr.push_str("\nunparseable stdout");
            return run;
        };
        let specific = reply.specific.unwrap_or_default();
        run.context = reply.context.or(specific.additional_context);
        run.message = reply.system_message;
        // A hook's own reason in whichever field it used, else the operator's
        // configured `text`, else the generic line `run` supplies. Capped, and
        // logged verbatim.
        run.reason = reply
            .reason
            .or(specific.permission_reason)
            .or(reply.stop_reason)
            .or_else(|| self.text.clone())
            .map(|mut r| {
                truncate_on_a_char_boundary(&mut r, HOOK_REASON_CAP);
                r
            });
        // Every spelling of "no", in one expression, because the failure to
        // avoid is a file that says deny and a tool that ran. `continue: false`
        // is first because Claude Code documents it as outranking the decision.
        let refused = reply.keep_going == Some(false)
            || matches!(reply.decision.as_deref(), Some("deny") | Some("block"))
            || matches!(
                specific.permission_decision.as_deref(),
                Some("deny") | Some("ask")
            );
        run.outcome = if refused {
            HookOutcome::Deny
        } else {
            HookOutcome::Allow
        };
        run
    }
}

/// `String::truncate` panics on a byte index inside a character, and a hook's
/// reason is arbitrary text from an arbitrary program — a 400-byte cut through a
/// `→` would take the process down at the exact moment a policy was being
/// explained. Cuts at the last boundary at or below the cap instead.
fn truncate_on_a_char_boundary(s: &mut String, cap: usize) {
    if s.len() > cap {
        let mut end = cap;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
}

/// A child process configured the only way this crate ever configures one:
/// **argv exec of an already-contained path — no shell, no PATH lookup, no
/// argument string for anything to be interpolated into — with the environment
/// cleared down to [`HOOK_ENV_ALLOWLIST`].**
///
/// Shared with the status line rather than copied, and that sharing is the
/// point. This process holds `ANTHROPIC_API_KEY`; a second spawn site that
/// forgot the `env_clear` would hand a config-named program the ability to
/// spend the user's money, and the way to not have a second spawn site that
/// forgot something is to not have a second spawn site.
pub(crate) fn contained_command(program: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
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
    cmd
}

/// How long the pipes get to finish draining **after** the hook has already
/// exited, before Emma takes what it has and walks away.
///
/// **Why a budget and not an event.** A hook may start something meant to
/// outlive it — that is a supported use, ruled 2026-08-14 — and that grandchild
/// inherits the same stdout. So there is no longer any observable event meaning
/// "the hook's own output is complete": end-of-file arrives when the *last*
/// writer closes, which may be days later, and a pipe cannot say which writer a
/// byte came from. What is knowable is that everything the hook itself wrote
/// completed into the pipe buffer before `exit` could run, so a grace long
/// enough to drain a local buffer captures all of it. 200ms is orders of
/// magnitude more than that and a tenth of the tightest caller's whole budget
/// (the status line's 2s), so in the overwhelmingly common case — no
/// grandchild, EOF already there — it costs nothing measurable.
///
/// A daemon's bytes that happen to land inside the grace ride along. That is
/// bounded by [`HOOK_OUTPUT_CAP`] and unavoidable without attribution the pipe
/// does not offer.
///
/// **Say what could not be verified: on Windows nothing here is pinned by a
/// test, because nothing could make its removal observable.** Deleting this
/// grace entirely leaves every fixture in `tests/exec_lifetime.rs` green,
/// including a 100-trial probe of a hook whose last write and exit are the same
/// instant (0/100 lost either way). The mechanism explains it — tokio gives a
/// child's stdio *overlapped* I/O on Windows, so the read is posted before the
/// bytes exist and completes into our buffer as they arrive, ahead of the exit
/// notification. On unix the same code is readiness-based: the bytes wait in the
/// kernel for a poll that `select!`, which randomises its branches, may not
/// reach before the wait wins. That is the platform where this constant is
/// expected to earn its place and the one where the probe should be run. Until
/// somebody runs it there, this is a reasoned bound with a measured cost of
/// zero, not a demonstrated need.
const EXEC_DRAIN_GRACE_MS: u64 = 200;

/// Spawn, feed stdin, wait for the hook, take what it wrote, walk away.
///
/// **The order is the design, and it was the defect.** This used to read stdout
/// to end-of-file and *then* wait. A hook that started a background process and
/// exited 0 in milliseconds was therefore reported as `timed out after 5000ms`
/// with its output discarded — not because anything was slow, but because the
/// process it started inherited the write end of the pipe and EOF never came.
/// Completion is the child's own exit; the pipes are drained around it, never
/// waited on in front of it. See `notes/plans/process-lifetime.md` §2.
///
/// **What Emma claims, and what it does not.** It supervises the direct child it
/// spawned — its budget, its pipes, its exit code — and claims nothing about
/// that child's descendants. On the timeout path `kill_on_drop` kills the hook
/// and only the hook. Whatever the hook started is left alone, because Emma
/// cannot tell a broken hook's orphans from the daemon the operator meant to
/// start, and under the ruling it must not guess.
///
/// **A note for hook authors: redirect a daemon's stdio.** Emma drops its read
/// end here. On Windows that is nothing to the daemon — a write into a broken
/// pipe is an error return it may ignore. On unix it is a `SIGPIPE`, which kills
/// a process that has not asked otherwise. Inherited stdio is borrowed, which is
/// why every daemon guide ever written says to point it at `/dev/null` (or
/// `NUL`) first; the alternative — Emma keeping a reader alive for as long as
/// somebody else's daemon lives — is an unbounded obligation and is refused.
///
/// A hook that writes past [`HOOK_OUTPUT_CAP`] blocks and hits the timeout,
/// which is still the right answer for a program that will not stop.
pub(crate) async fn exec(
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
    let (mut so, mut se) = (child.stdout.take(), child.stderr.take());

    let status;
    {
        // Both pipes, concurrently with the wait. The buffers are borrowed
        // rather than owned by the futures precisely so that abandoning a read
        // keeps the bytes it already collected.
        let both = async {
            tokio::join!(
                async {
                    if let Some(r) = so.as_mut() {
                        let _ = drain(r, &mut out).await;
                    }
                },
                async {
                    if let Some(r) = se.as_mut() {
                        let _ = drain(r, &mut err).await;
                    }
                }
            );
        };
        tokio::pin!(both);
        let mut exited = None;
        tokio::select! {
            // Both pipes reached EOF (or the cap) first: the ordinary case with
            // no grandchild anywhere, and `wait` below returns immediately.
            _ = &mut both => {}
            s = child.wait() => exited = Some(s?),
        }
        status = match exited {
            Some(s) => {
                // The hook is done. Give whatever is still in the pipes a
                // moment to arrive, then stop: a write end we do not own may
                // stay open indefinitely and is not ours to wait for.
                let _ = tokio::time::timeout(Duration::from_millis(EXEC_DRAIN_GRACE_MS), &mut both)
                    .await;
                s
            }
            None => child.wait().await?,
        };
    }

    // Emma's read ends, closed. The write ends belong to whatever still holds
    // them and die with it; nothing is retained here.
    drop(so);
    drop(se);
    Ok((status.code(), out, err))
}

/// Read until end-of-file or [`HOOK_OUTPUT_CAP`], into a buffer the caller owns.
///
/// Hand-rolled rather than `take(cap).read_to_end(buf)` because this future is
/// **abandoned** when the grace expires, and `read_to_end` is documented as not
/// cancellation-safe: what it has read so far is not guaranteed to be in the
/// buffer. Here the boundary is one `read` call, and every byte that returned
/// from one is already appended.
async fn drain<R>(r: &mut R, buf: &mut Vec<u8>) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut chunk = [0u8; 8 * 1024];
    while (buf.len() as u64) < HOOK_OUTPUT_CAP {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        let room = HOOK_OUTPUT_CAP as usize - buf.len();
        buf.extend_from_slice(&chunk[..n.min(room)]);
    }
    Ok(())
}

// endregion: Dispatch

// region: Resolution — every check that can be made before a hook ever runs
// ---------------------------------------------------------------------------
// Resolution — every check that can be made before a hook ever runs
//
// Load time, where a hook that cannot be run safely stops the boot instead of
// failing at the moment it was needed. Everything here answers the same
// question: is there any way to know now that this will not work?
// ---------------------------------------------------------------------------

/// Turn a config-declared command into a path that is safe to exec, or say why
/// it is not. The three checks are the boundary, and they are shared rather
/// than repeated: **anything config can point Emma at goes through here.**
///
/// The containment check is why `command` is relative — an absolute path or a
/// `..` escape would let a config file run any executable on the box with
/// Emma's permissions, and Emma's permissions include writing the user's source
/// tree. The executable-bit check is not the boundary; it catches the operator
/// who forgot to `chmod +x`, which would otherwise surface much later as a
/// spawn failure that reads like something else.
///
/// `what` names the caller in the sentence, because "no such command" with no
/// subject is a sentence an operator cannot act on.
pub(crate) fn contain(root: &Path, what: &str, command: &str) -> Result<PathBuf, String> {
    let dir = root
        .join("hooks")
        .canonicalize()
        .map_err(|_| format!("{what} is defined but hooks/ is missing"))?;
    let path = root
        .join(command)
        .canonicalize()
        .map_err(|_| format!("{what}: no such command `{command}`"))?;
    if !path.starts_with(&dir) {
        return Err(format!(
            "{what} resolves to {}, outside {}",
            path.display(),
            dir.display()
        ));
    }
    // Unix only, because there is no executable bit on Windows to consult — a
    // `.cmd` or `.exe` is runnable by extension.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&path).map_err(|e| format!("{what}: {e}"))?;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(format!("{what} is not executable"));
        }
    }
    Ok(path)
}

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
        let event =
            HookEvent::parse(&def.event).with_context(|| named(format!("hook `{name}`")))?;
        let command = contain(root, &format!("hook `{name}`"), &def.command)
            .map_err(|e| anyhow!(named(e)))?;
        // A `UserPromptSubmit` hook has no tool name to match, so a matcher on
        // one is a filter that can never be true. Claude Code ignores the field
        // here; Emma names it, on the same rule as the unimplemented event — a
        // hook the operator believes is conditional and which in fact never
        // fires is worse than one that always does. The empty string is the
        // exception and is treated as absent: `"matcher": ""` is what a group
        // written for a tool event looks like when it was copied for this one,
        // and it asks for nothing.
        let matcher_text = def.matcher.as_deref().filter(|m| !m.trim().is_empty());
        if event == HookEvent::UserPromptSubmit {
            if let Some(m) = matcher_text {
                bail!(named(format!(
                    "hook `{name}` is a UserPromptSubmit hook with matcher `{m}`, but there is \
                     no tool name to match on this event — remove the matcher, or match inside \
                     the hook on the `prompt` field it is given"
                )));
            }
        }
        let matcher = match &def.matcher {
            // Only an empty one can have survived the check above, and on this
            // event it means nothing was asked for.
            Some(_) if event == HookEvent::UserPromptSubmit => None,
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

// endregion: Resolution — every check that can be made before a hook ever runs
