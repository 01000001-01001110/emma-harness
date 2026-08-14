//! `statusLine`: a program named by configuration whose stdout becomes the
//! bottom row of the screen.
//!
//! This is Claude Code's feature and Claude Code's contract, implemented here so
//! that a `settings.json` somebody already wrote keeps working — the whole point
//! is that an existing configuration is not rewritten for Emma. What Emma
//! borrows is the shape (`type`/`command`/`padding`), the JSON handed to the
//! program on stdin, and the rule that its stdout is displayed as-is, colour and
//! all. See `notes/status-line.md` for what Emma sends, what it cannot, and what
//! is lost by configuring one at all.
//!
//! # Why this lives beside `hooks.rs` and not near the terminal
//!
//! **It is arbitrary code from a config file, and it bypasses the approval gate
//! by design — exactly as a hook does.** The user is asked before Emma runs
//! `Bash`; nobody is asked before Emma runs this. That is the same trade a hook
//! makes and it is defensible for the same reason: the person who wrote
//! `settings.json` is the person at the keyboard. What is not defensible is
//! making that trade *twice, differently*, so everything here reuses the hook
//! supervisor rather than growing a second one:
//!
//! - [`hooks::contain`] — the path must canonicalise inside `<root>/hooks/`.
//! - [`hooks::contained_command`] — argv exec, no shell, environment cleared to
//!   six names, `kill_on_drop`.
//! - [`hooks::exec`] — waits on the child, drains both pipes under a hard cap
//!   around that wait, and claims nothing about what the child started.
//!
//! **That last one carries a ruling this file inherits rather than makes.** A
//! program that spawns something meant to outlive it is a supported use (owner,
//! 2026-08-14), so `exec` abandons the pipe instead of waiting for an
//! end-of-file the daemon holds open — which is what stops a status script that
//! exited 0 from being reported as `timed out after 2000ms` on every debounced
//! repaint. The *other* half of that ruling sits less comfortably here than it
//! does on a hook: a status script runs many times a session, so one that
//! daemonizes accumulates a process per burst. That is flagged for a second
//! owner ruling in `notes/plan-process-lifetime.md` §4 and is deliberately
//! **not** decided by growing `exec` a caller-chosen policy — one behaviour for
//! both until somebody rules otherwise, for the same reason everything else on
//! this list is shared.
//!
//! **The one place Emma refuses what Claude Code accepts.** There, `command`
//! runs in a shell, so `jq -r '...'` inline is idiomatic. Here it cannot: Emma
//! execs a contained argv precisely so a config file cannot name an arbitrary
//! executable, and honouring a shell string would drop that at the exact point
//! where the same file's hooks are relied on to hold it. `claude::
//! translate_command` already owns that ruling and its sentence, and this uses
//! it unchanged.
//!
//! **The one place Emma is softer than it is about hooks.** A hook that cannot
//! be resolved stops the boot, because a policy the operator believes they have
//! and does not is worse than none. A status line that cannot be resolved is
//! *cosmetic*: it is noted, and the built-in status is drawn instead. Refusing
//! to start over the decoration on the bottom row would be the outage this crate
//! keeps ruling against.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::hash;
use crate::hooks;

// region: The declaration — the statusLine block of settings.json
// ---------------------------------------------------------------------------
// The declaration — the `statusLine` block of `settings.json`
// ---------------------------------------------------------------------------

/// **Permissive, unlike the hooks block, and the difference is the point.** A
/// misspelled key inside `hooks` silently changes who a policy guards, so that
/// block is `deny_unknown_fields`. This block carries `refreshInterval`,
/// `hideVimModeIndicator` and whatever Claude Code adds next; every one of them
/// is a display preference, and refusing to boot over one would make this
/// compatibility feature an obstacle. Same ruling as the outer object, one level
/// down. See `claude.rs`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct StatusLineBlock {
    #[serde(rename = "type", default)]
    pub(crate) kind: Option<String>,
    #[serde(default)]
    pub(crate) command: Option<String>,
    /// Extra leading spaces. Claude Code counts it in characters and defaults it
    /// to zero.
    #[serde(default)]
    pub(crate) padding: Option<u16>,
}

/// How long the program gets before it is killed.
///
/// **Not configurable, and that is deliberate: Claude Code has no such field, so
/// inventing one would be a key that means nothing in the file it was copied
/// from.** The number is small because of where the value is consumed — a row
/// that is repainted while somebody is typing. Two seconds is long enough for a
/// `git status` on a large repository and short enough that a program which will
/// never answer stops being asked.
const STATUS_TIMEOUT_MS: u64 = 2_000;

// endregion: The declaration — the statusLine block of settings.json

// region: The resolved command
// ---------------------------------------------------------------------------

/// A status-line program that survived every check that can be made before it is
/// ever run.
#[derive(Debug, Clone)]
pub struct StatusLine {
    command: PathBuf,
    /// Identity of what will run, for `Harness::snapshot`. Same rule as a hook:
    /// a log that records a name rather than a hash cannot answer "was this the
    /// program that ran".
    command_hash: String,
    timeout: Duration,
    /// Leading spaces the configuration asked for.
    pub padding: u16,
}

impl StatusLine {
    /// Resolve the block, or say why not.
    ///
    /// `Ok(None)` is "nothing was configured", which is the ordinary case and not
    /// a problem. `Err` is a sentence for the user — it never stops the boot; see
    /// the module docs.
    pub(crate) fn resolve(
        root: &Path,
        block: Option<StatusLineBlock>,
    ) -> Result<Option<Self>, String> {
        let Some(block) = block else {
            return Ok(None);
        };
        // An absent `command` is the same as an absent block: nothing was asked
        // for. An empty one is a typo worth naming.
        let Some(raw) = block.command else {
            return Ok(None);
        };
        if raw.trim().is_empty() {
            return Err("statusLine has an empty `command`".into());
        }
        // Claude Code's only type is `command`. Anything else is a file written
        // for a program with more of them, and guessing would be worse than
        // saying so.
        match block.kind.as_deref() {
            None | Some("command") => {}
            Some(other) => {
                return Err(format!(
                    "statusLine has type `{other}`; Emma runs command status lines only"
                ))
            }
        }
        // The same translation, refusal and sentence a hook command gets. A
        // shell string lands here and is turned down by name.
        let relative = crate::claude::translate_command(root, "statusLine", &raw)
            .map_err(|e| e.to_string())?;
        let command = hooks::contain(root, "statusLine", &relative)?;
        let command_hash = std::fs::read(&command)
            .map(|b| hash::short(&String::from_utf8_lossy(&b)))
            .map_err(|e| format!("statusLine: {} could not be read: {e}", command.display()))?;
        Ok(Some(Self {
            command,
            command_hash,
            timeout: Duration::from_millis(STATUS_TIMEOUT_MS),
            padding: block.padding.unwrap_or(0),
        }))
    }

    /// For `Harness::snapshot`: what will run, never its text.
    pub fn identity(&self) -> serde_json::Value {
        serde_json::json!({ "command": self.command, "hash": self.command_hash })
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Run it once and return what it printed.
    ///
    /// **This function's whole contract is that it returns.** It is called on
    /// behalf of a repaint, so a program that hangs must become an `Err` on a
    /// timer rather than a terminal nobody can type into. The timeout is the
    /// engine's, the pipes are capped, and the child is killed on drop — all
    /// three inherited from the hook supervisor rather than re-decided here.
    ///
    /// `columns` and `rows` become `COLUMNS` and `LINES`, which is how Claude
    /// Code tells a status script how wide it may draw: the script's stdout is
    /// captured rather than attached to the terminal, so nothing inside it can
    /// ask the terminal directly.
    pub async fn run(
        &self,
        payload: &StatusPayload,
        columns: u16,
        rows: u16,
    ) -> Result<String, String> {
        let mut cmd = hooks::contained_command(&self.command);
        cmd.env("COLUMNS", columns.to_string());
        cmd.env("LINES", rows.to_string());
        let body = serde_json::to_string(payload).map_err(|e| e.to_string())?;
        let finished = tokio::time::timeout(self.timeout, hooks::exec(&mut cmd, body)).await;
        let (code, stdout, stderr) = match finished {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(format!("statusLine could not be run: {e}")),
            Err(_) => {
                return Err(format!(
                    "statusLine timed out after {}ms",
                    self.timeout.as_millis()
                ))
            }
        };
        if code != Some(0) {
            // stderr is the only thing a failing script has to explain itself
            // with, so it goes in the sentence rather than to a log nobody
            // reads. One line of it: this is a note on a terminal.
            let why = String::from_utf8_lossy(&stderr);
            let why = why.lines().next().unwrap_or("").trim();
            return Err(match (code, why.is_empty()) {
                (Some(c), false) => format!("statusLine exited {c}: {why}"),
                (Some(c), true) => format!("statusLine exited {c}"),
                (None, _) => "statusLine was killed by a signal".to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }
}

// endregion: The resolved command

// region: What the program is told
// ---------------------------------------------------------------------------
// What the program is told
//
// Claude Code's stdin payload, restricted to the fields Emma can answer
// truthfully. The field *names* are not ours to choose — a script written
// against `.model.display_name` has to keep working — but which fields exist is,
// and the rule is the one the status line itself follows: a value nobody
// measured is absent rather than zero. Emma has no dollar cost, no line counts,
// no rate limits and no PR, so it sends none of those; Claude Code's own schema
// documents most of them as fields that may be absent, and `jq`'s `//` fallback
// is the idiom every example already uses.
// ---------------------------------------------------------------------------

/// Everything Emma can honestly tell a status-line program.
#[derive(Debug, Default, Clone, Serialize)]
pub struct StatusPayload {
    pub cwd: String,
    pub session_id: String,
    pub transcript_path: String,
    pub model: StatusModel,
    pub workspace: StatusWorkspace,
    /// Emma's version, in Claude Code's field. A script that prints it will say
    /// what it is actually running under, which is the useful answer.
    pub version: String,
    pub cost: StatusCost,
    /// `None` until a model call has reported one — see [`StatusContext`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<StatusContext>,
    pub exceeds_200k_tokens: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct StatusModel {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct StatusWorkspace {
    pub current_dir: String,
    pub project_dir: String,
    /// Always present, always empty: Emma has no `/add-dir`. Claude Code
    /// documents it as an empty array when nothing was added, so a script
    /// iterating it finds the same shape rather than a null.
    pub added_dirs: Vec<String>,
}

/// What the run has spent. Emma computes no dollar figure, so it sends none —
/// `total_cost_usd` absent is a `// 0` in somebody's `jq`, and a fabricated zero
/// is a number they would believe.
#[derive(Debug, Default, Clone, Serialize)]
pub struct StatusCost {
    pub total_duration_ms: u64,
}

/// The live context window, in Claude Code's spelling.
///
/// Emma measures the provider's own input count against the compaction cap, so
/// `total_input_tokens`, `context_window_size` and the two percentages are real.
/// `total_output_tokens` and `current_usage` are not tracked per-call here and
/// are therefore absent rather than guessed.
#[derive(Debug, Default, Clone, Serialize)]
pub struct StatusContext {
    pub total_input_tokens: i64,
    pub context_window_size: i64,
    pub used_percentage: i64,
    pub remaining_percentage: i64,
}

impl StatusContext {
    /// Both percentages from one measurement, so they cannot disagree.
    pub fn new(used: i64, cap: i64) -> Self {
        let pct = if cap > 0 {
            (used.saturating_mul(100) / cap).clamp(0, 100)
        } else {
            0
        };
        Self {
            total_input_tokens: used,
            context_window_size: cap,
            used_percentage: pct,
            remaining_percentage: 100 - pct,
        }
    }
}

// endregion: What the program is told

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_percentages_are_one_measurement_and_always_agree() {
        for (used, cap) in [(0, 200_000), (50_000, 200_000), (199_999, 200_000), (7, 3)] {
            let c = StatusContext::new(used, cap);
            assert_eq!(
                c.used_percentage + c.remaining_percentage,
                100,
                "used {used} of {cap} produced percentages that do not sum to 100"
            );
        }
        // A cap of zero is "no cap", not a division by zero — the same ruling
        // the built-in meter makes.
        let none = StatusContext::new(5, 0);
        assert_eq!(none.used_percentage, 0);
        assert_eq!(none.remaining_percentage, 100);
        // …and a run past its cap reads as full rather than as more than full.
        assert_eq!(StatusContext::new(400_000, 200_000).used_percentage, 100);
    }

    /// The rule the whole payload is written to: a field nobody measured is
    /// absent from the JSON, not zero in it. A script's `// 0` can then supply a
    /// default the script's author chose.
    #[test]
    fn a_field_emma_cannot_measure_is_absent_from_the_payload_rather_than_zero() {
        let json = serde_json::to_value(StatusPayload::default()).expect("serialise");
        assert!(
            json.get("context_window").is_none(),
            "a context window was reported before anything measured one: {json}"
        );
        // Nothing invents a dollar figure, a line count or a rate limit.
        for absent in ["total_cost_usd", "total_lines_added", "total_lines_removed"] {
            assert!(
                json["cost"].get(absent).is_none(),
                "{absent} was fabricated: {json}"
            );
        }
        for absent in ["rate_limits", "pr", "worktree", "vim", "output_style"] {
            assert!(
                json.get(absent).is_none(),
                "{absent} was fabricated: {json}"
            );
        }
    }

    /// The field names are Claude Code's and are not ours to improve. A script
    /// written against `.model.display_name` and `.workspace.current_dir` has to
    /// keep working, which is the entire reason this feature exists.
    #[test]
    fn the_payload_uses_claude_codes_field_names_because_existing_scripts_read_them() {
        let payload = StatusPayload {
            cwd: "E:\\emma".into(),
            session_id: "sess-1".into(),
            transcript_path: "E:\\logs\\sess-1.jsonl".into(),
            model: StatusModel {
                id: "claude-opus-4".into(),
                display_name: "claude-opus-4".into(),
            },
            workspace: StatusWorkspace {
                current_dir: "E:\\emma".into(),
                project_dir: "E:\\emma".into(),
                added_dirs: Vec::new(),
            },
            version: "0.1.0".into(),
            cost: StatusCost {
                total_duration_ms: 1_500,
            },
            context_window: Some(StatusContext::new(12_000, 120_000)),
            exceeds_200k_tokens: false,
        };
        let j = serde_json::to_value(&payload).expect("serialise");
        assert_eq!(j["model"]["display_name"], "claude-opus-4");
        assert_eq!(j["workspace"]["current_dir"], "E:\\emma");
        assert_eq!(j["workspace"]["added_dirs"], serde_json::json!([]));
        assert_eq!(j["session_id"], "sess-1");
        assert_eq!(j["transcript_path"], "E:\\logs\\sess-1.jsonl");
        assert_eq!(j["cost"]["total_duration_ms"], 1_500);
        assert_eq!(j["context_window"]["used_percentage"], 10);
        assert_eq!(j["exceeds_200k_tokens"], false);
    }

    /// `type` is the one key in the block Emma has an opinion about, and the
    /// opinion is the same one the hooks block holds: a type this program cannot
    /// honour is said out loud rather than guessed at.
    #[test]
    fn a_status_line_of_a_type_emma_cannot_run_is_named_rather_than_guessed() {
        let block = StatusLineBlock {
            kind: Some("wasm".into()),
            command: Some("hooks/status.sh".into()),
            padding: None,
        };
        let err = StatusLine::resolve(Path::new("."), Some(block)).unwrap_err();
        assert!(err.contains("wasm"), "{err}");
        assert!(err.contains("command status lines only"), "{err}");
    }

    /// Nothing configured is not a problem, and neither is a block with no
    /// command in it. Both mean "draw the built-in", which is what Emma did
    /// before this file existed.
    #[test]
    fn no_status_line_configured_is_not_an_error() {
        assert!(StatusLine::resolve(Path::new("."), None)
            .expect("absent is fine")
            .is_none());
        assert!(StatusLine::resolve(
            Path::new("."),
            Some(StatusLineBlock {
                kind: Some("command".into()),
                command: None,
                padding: None,
            })
        )
        .expect("a block with no command is fine")
        .is_none());
    }

    /// The refusal that makes this feature safe, and the one place Emma is
    /// deliberately narrower than Claude Code.
    ///
    /// Claude Code runs `command` in a shell, so its own documentation's second
    /// example is an inline `jq` pipeline. Emma execs a contained argv so that a
    /// config file cannot name an arbitrary executable — the rule its hooks
    /// depend on — and it cannot honour both. The sentence has to say what to do
    /// instead, because an operator who copied a working config is not wrong,
    /// only unportable.
    #[test]
    fn an_inline_shell_command_is_refused_with_a_sentence_that_says_what_to_do() {
        let inline = r#"jq -r '"[\(.model.display_name)]"'"#;
        let err = StatusLine::resolve(
            Path::new("."),
            Some(StatusLineBlock {
                kind: Some("command".into()),
                command: Some(inline.into()),
                padding: None,
            }),
        )
        .unwrap_err();
        assert!(err.contains("shell command"), "{err}");
        assert!(err.contains("hooks/"), "the fix was not stated: {err}");
    }

    /// An absolute path is the other half of the same boundary. `hooks/` is the
    /// only place a config file may point at, so `/usr/bin/whatever` is turned
    /// down here exactly as it is for a hook.
    #[test]
    fn an_absolute_path_is_refused_the_way_a_hook_command_is() {
        for absolute in ["/usr/local/bin/status", "C:\\tools\\status.exe"] {
            let err = StatusLine::resolve(
                Path::new("."),
                Some(StatusLineBlock {
                    kind: Some("command".into()),
                    command: Some(absolute.into()),
                    padding: None,
                }),
            )
            .unwrap_err();
            assert!(
                err.contains("absolute path") || err.contains("shell command"),
                "{absolute} was not refused: {err}"
            );
        }
    }
}
