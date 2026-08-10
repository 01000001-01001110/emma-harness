//! The approval gate.
//!
//! A tool surface that is read-only by construction bounds the worst a rogue
//! turn can do to reading something it was entitled to read, and needs no gate
//! to achieve it. Emma writes files and runs commands, which inverts that
//! property, and this file is the thing standing in its place.
//!
//! **The rules, in the order they are applied, because the order is the
//! design.**
//!
//! 1. **A `PreToolUse` hook that denies wins.** It is checked before any human
//!    is asked, and no answer overrides it — not a `y`, not a session
//!    allowance, not `--dangerously-skip-permissions`. A hook is policy the
//!    operator wrote down; the prompt is convenience for the person sitting
//!    there. If a human could wave a hook through, the hook would be advice.
//!    (The check itself lives in the loop, which is where the hook runner is.
//!    What is enforced here is that nothing in this file can undo it.)
//! 2. **`ToolMeta::read_only` decides whether to ask at all.** Read, Glob and
//!    Grep run silently; Write, Edit and Bash ask. Prompting for reads is how
//!    an agent becomes unusable in under a minute, and an unusable gate gets
//!    turned off.
//! 3. **The prompt shows what will actually happen** — the command, the diff,
//!    the path and size. A prompt the user cannot evaluate trains them to press
//!    `y`, which is worse than no prompt: it manufactures consent and leaves a
//!    record saying they agreed.
//!
//! **What is deliberately absent.** There is no persistent always-allow. "Yes,
//! and stop asking for this tool" lives in a `HashSet` on this struct and dies
//! with the process — a permission the user cannot see is a permission they
//! have forgotten they granted, and the place they would not see it is a config
//! file written six weeks ago. The session scope is the longest scope a
//! permission may have.
//!
//! **The bypass.** `--dangerously-skip-permissions` exists because scripting
//! exists. It cannot be set from configuration or the environment, it is
//! announced loudly at startup by the caller, and every call it waves through
//! is still written to the session log. The short `--yes` spelling is accepted
//! only alongside `-p`, so the interactive path cannot reach the bypass without
//! typing the whole word.

use std::collections::HashSet;

use emma_tool_api::{Tool, ToolMeta};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::term::{LineSource, Term};

// region: The exemption
// ---------------------------------------------------------------------------
// The exemption
//
// The one place the gate is deliberately weakened, given its own section so it
// cannot be read past. Everything below this exists to make the gate hard to
// weaken; this is the exception, with the argument attached.
// ---------------------------------------------------------------------------

/// Writers that do not prompt, and the whole argument for each one.
///
/// **This is a hole in the gate, written as a list so that it is findable and
/// deletable rather than buried in a condition.** Everything else in this file
/// exists to make the gate hard to weaken; this is the one place it is
/// deliberately weakened, so it is stated at the top where a reviewer trips
/// over it.
///
/// `ToolMeta` has one axis and it answers "can this change anything?". The task
/// tools answer that honestly — they rewrite `.emma/tasks/tasks.md` — and they
/// were deliberately *not* declared `read_only: true` to dodge the gate, which
/// is correct: a tool that misreports itself leaves the gate protecting nothing
/// in general, not just for that tool.
///
/// But the write is confined to the tool's own bookkeeping file under `.emma/`,
/// and it is the write a model performs several times per goal. This file's own
/// design says a prompt the user cannot evaluate is worse than no prompt
/// because it manufactures consent — and "TaskUpdate wants to tick a checkbox,
/// allow?" fifteen times in one goal is precisely the prompt that teaches
/// somebody to hold down `y`, including through the `Bash` call that comes
/// after it. Exempting these two protects the prompts that matter.
///
/// The missing thing is a second axis on `ToolMeta`: the *scope* of a write,
/// not merely its existence. That belongs in `tool-api`, which this crate does
/// not own. **When it lands, delete this list** — the tools will classify
/// themselves and there will be nothing here left to do.
///
/// Two things this does not weaken, and they are why it is survivable: a
/// `PreToolUse` hook denial is resolved in the loop *before* approvals are
/// consulted, so an operator can still block these; and every call, exempt or
/// not, is written to the session log.
const EXEMPT: &[&str] = &["TaskCreate", "TaskUpdate"];

// endregion: The exemption

// region: Answers, verdicts and gates
// ---------------------------------------------------------------------------
// Answers, verdicts and gates
//
// The vocabulary the gate decides in: what a human said, what the gate
// concluded, which mode it is running in, and where answers come from. The
// three modes are the whole policy surface — there is no fourth.
// ---------------------------------------------------------------------------

/// What a human answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    /// Yes, and stop asking for this tool — for this process only.
    AlwaysThisTool,
}

/// The gate's decision about one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Carries the sentence the model is told. A denial the model cannot read
    /// is a tool that mysteriously does nothing.
    Deny(String),
}

/// How approval is obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Ask the human. The default, and the only mode reachable without a flag.
    Ask,
    /// `--dangerously-skip-permissions`. Everything runs.
    SkipAll,
    /// `-p` with no bypass: there is nobody to ask, so anything needing
    /// approval is denied and the model is told why.
    ///
    /// The two alternatives are both worse. Proceeding silently makes `-p` the
    /// way to get unattended writes without saying so. Prompting anyway hangs
    /// on a question nobody can see, which in a CI job is a timeout an hour
    /// later with no output explaining it.
    Unattended,
}

/// Where answers come from.
pub enum Asker {
    Terminal(Mutex<LineSource>),
    /// Fixed answers, in order, for tests. Exists so the gate is exercised by
    /// the same code path a person drives — a gate tested through a mock of
    /// itself is a gate nobody has tested.
    Scripted(Mutex<Vec<Answer>>),
}

// endregion: Answers, verdicts and gates

// region: The gate
// ---------------------------------------------------------------------------
// The gate
//
// `decide` is the file. Its arms are in a deliberate order — read-only, the
// exemption, the bypass, the session allowance, then unattended — and every
// path that does not allow returns a sentence the model can act on.
// ---------------------------------------------------------------------------

pub struct Approvals {
    gate: Gate,
    asker: Asker,
    session_allowed: Mutex<HashSet<String>>,
    /// Everything the gate decided, in order, read back through
    /// [`Approvals::decisions`].
    ///
    /// No caller in this workspace calls `decisions` today, so this is
    /// currently write-only. The record a run actually depends on is the
    /// session log, which the loop writes on every denial and every call it
    /// lets through.
    seen: Mutex<Vec<(String, Verdict)>>,
}

impl Approvals {
    pub fn new(gate: Gate, asker: Asker) -> Self {
        Self {
            gate,
            asker,
            session_allowed: Mutex::new(HashSet::new()),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// `-p` with no bypass.
    pub fn unattended() -> Self {
        Self::new(Gate::Unattended, Asker::Scripted(Mutex::new(Vec::new())))
    }

    pub fn gate(&self) -> Gate {
        self.gate
    }

    pub async fn decisions(&self) -> Vec<(String, Verdict)> {
        self.seen.lock().await.clone()
    }

    /// Decide one call.
    ///
    /// Takes the `Tool` rather than a name and a bool so `read_only` is read
    /// from the tool that is about to run. A caller passing its own idea of
    /// whether something writes is the gate protecting a claim instead of a
    /// fact.
    pub async fn request(&self, tool: &dyn Tool, args: &Value, term: &Term) -> Verdict {
        let verdict = self.decide(tool.name(), tool.meta(), args, term).await;
        self.seen
            .lock()
            .await
            .push((tool.name().to_string(), verdict.clone()));
        verdict
    }

    async fn decide(&self, name: &str, meta: ToolMeta, args: &Value, term: &Term) -> Verdict {
        if meta.read_only {
            return Verdict::Allow;
        }
        // The named hole. See `EXEMPT`.
        if EXEMPT.contains(&name) {
            return Verdict::Allow;
        }
        if self.gate == Gate::SkipAll {
            return Verdict::Allow;
        }
        if self.session_allowed.lock().await.contains(name) {
            return Verdict::Allow;
        }
        if self.gate == Gate::Unattended {
            return Verdict::Deny(format!(
                "{name} needs approval and this is a non-interactive run (-p), so there is \
                 nobody to ask. It was not run. Use a read-only tool, or tell the user what \
                 needs approving and stop."
            ));
        }

        term.prompt_header(name, &preview(name, args));
        // `ask` loops on unreadable input; by the time it answers, the answer
        // is one of the three.
        match self.ask(name, term).await {
            Some(Answer::Yes) => Verdict::Allow,
            Some(Answer::AlwaysThisTool) => {
                self.session_allowed.lock().await.insert(name.to_string());
                term.note(&format!(
                    "{name} is approved for the rest of this session (this process only)"
                ));
                Verdict::Allow
            }
            Some(Answer::No) => Verdict::Deny(format!(
                "The user declined this {name} call. It was not run. Do not retry it \
                 unchanged — take a different approach, or ask what they would prefer."
            )),
            // End of input, or a scripted run that ran out of answers. Silence
            // is not consent.
            None => Verdict::Deny(format!(
                "No approval was given for this {name} call, so it was not run."
            )),
        }
    }

    /// One line from the same queue approvals are answered on.
    ///
    /// The goal prompt reads through here rather than opening its own reader:
    /// two readers on one stdin race, and the loser buffers the answer to a
    /// question the winner asked — which, for a gate, means a `y` intended for
    /// nothing at all.
    pub async fn read_line(&self) -> Option<String> {
        match &self.asker {
            Asker::Terminal(lines) => lines.lock().await.next().await,
            Asker::Scripted(_) => None,
        }
    }

    async fn ask(&self, name: &str, term: &Term) -> Option<Answer> {
        match &self.asker {
            Asker::Scripted(queue) => {
                let mut q = queue.lock().await;
                if q.is_empty() {
                    return None;
                }
                Some(q.remove(0))
            }
            Asker::Terminal(lines) => {
                let mut lines = lines.lock().await;
                loop {
                    term.prompt_question(name);
                    let line = lines.next().await?;
                    match line.trim().to_ascii_lowercase().as_str() {
                        "y" | "yes" => return Some(Answer::Yes),
                        // Empty is *not* yes. A user who hits return to get
                        // their prompt back has not read anything.
                        "" | "n" | "no" => return Some(Answer::No),
                        "a" | "always" => return Some(Answer::AlwaysThisTool),
                        other => term.note(&format!("`{other}` is not one of y / n / a")),
                    }
                }
            }
        }
    }
}

// endregion: The gate

// region: What the human is shown
// ---------------------------------------------------------------------------
// What the human is shown
//
// A prompt is only worth the information in it. One arm per writing tool, and
// a diff that is the tool's own arguments rather than a computed one.
// ---------------------------------------------------------------------------

/// What the human is shown. The whole point of the gate.
///
/// One arm per tool that can change something, because a generic JSON dump is
/// exactly the prompt people learn to approve without reading. Anything without
/// an arm falls back to pretty JSON — visibly worse, which is the right
/// pressure on whoever adds the next writing tool.
pub fn preview(tool: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
    match tool {
        "Bash" => {
            let mut out = format!("$ {}", s("command"));
            if let Some(cwd) = args.get("cwd").and_then(Value::as_str) {
                out.push_str(&format!("\n  in {cwd}"));
            }
            out
        }
        "Write" => {
            let content = s("content");
            format!(
                "write {}\n  {} bytes, {} lines",
                s("file_path"),
                content.len(),
                content.lines().count()
            )
        }
        "Edit" => {
            let all = args
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            format!(
                "edit {}{}\n{}",
                s("file_path"),
                if all { "  (every occurrence)" } else { "" },
                diff(s("old_string"), s("new_string"))
            )
        }
        _ => serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string()),
    }
}

/// The exact text going out and the exact text coming in, as `-`/`+` lines.
///
/// Not a computed diff, and it must not become one. `Edit`'s arguments *are*
/// the two sides, so showing them verbatim is the only rendering that cannot
/// disagree with what the tool will do. A minimised diff would be prettier and
/// would show the user a summary of the change rather than the change.
const DIFF_LINES: usize = 40;

fn diff(old: &str, new: &str) -> String {
    let mut out = String::new();
    let mut shown = 0usize;
    let push = |sign: char, text: &str, out: &mut String, shown: &mut usize| {
        for line in text.lines() {
            if *shown == DIFF_LINES {
                out.push_str("  …\n");
                *shown += 1;
                return;
            }
            if *shown > DIFF_LINES {
                return;
            }
            out.push_str(&format!("  {sign} {line}\n"));
            *shown += 1;
        }
    };
    push('-', old, &mut out, &mut shown);
    push('+', new, &mut out, &mut shown);
    out.trim_end().to_string()
}

// endregion: What the human is shown

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// `decide` is exercised directly, and the exemption test is written about what
// is *not* on the list — the hazard is a future edit adding a tool that writes
// the user's source tree.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_read_only_tool_is_never_asked_about_even_unattended() {
        let a = Approvals::unattended();
        let meta = ToolMeta {
            read_only: true,
            idempotent: true,
        };
        assert_eq!(
            a.decide("Read", meta, &Value::Null, &Term::silent()).await,
            Verdict::Allow
        );
    }

    #[tokio::test]
    async fn unattended_denies_a_writer_and_says_why() {
        let a = Approvals::unattended();
        let meta = ToolMeta {
            read_only: false,
            idempotent: false,
        };
        match a.decide("Bash", meta, &Value::Null, &Term::silent()).await {
            Verdict::Deny(why) => assert!(why.contains("-p"), "{why}"),
            Verdict::Allow => panic!("an unattended run approved a shell command"),
        }
    }

    #[tokio::test]
    async fn always_is_scoped_to_the_tool_and_to_the_process() {
        let a = Approvals::new(Gate::Ask, Asker::Scripted(Mutex::new(vec![Answer::AlwaysThisTool])));
        let write = ToolMeta {
            read_only: false,
            idempotent: false,
        };
        assert_eq!(
            a.decide("Write", write, &Value::Null, &Term::silent()).await,
            Verdict::Allow
        );
        // The second Write needs no answer — the allowance covers it.
        assert_eq!(
            a.decide("Write", write, &Value::Null, &Term::silent()).await,
            Verdict::Allow
        );
        // …and covers nothing else. The queue is empty, so a different tool
        // gets the no-answer denial rather than riding on Write's allowance.
        assert!(matches!(
            a.decide("Bash", write, &Value::Null, &Term::silent()).await,
            Verdict::Deny(_)
        ));
    }

    #[tokio::test]
    async fn the_exemption_covers_the_task_writers_and_nothing_that_touches_the_tree() {
        let a = Approvals::unattended();
        let writes = ToolMeta {
            read_only: false,
            idempotent: false,
        };
        for exempt in EXEMPT {
            assert_eq!(
                a.decide(exempt, writes, &Value::Null, &Term::silent()).await,
                Verdict::Allow,
                "{exempt} is listed as exempt but was gated"
            );
        }
        // The list is the hazard, so the test is about what is *not* on it. A
        // future edit that quietly adds a tool which writes the user's source
        // tree is the failure this exists to catch.
        for gated in ["Bash", "Write", "Edit"] {
            assert!(
                !EXEMPT.contains(&gated),
                "{gated} writes the working tree and must never be exempt"
            );
            assert!(
                matches!(
                    a.decide(gated, writes, &Value::Null, &Term::silent()).await,
                    Verdict::Deny(_)
                ),
                "{gated} was not gated"
            );
        }
    }

    #[tokio::test]
    async fn running_out_of_answers_denies() {
        let a = Approvals::new(Gate::Ask, Asker::Scripted(Mutex::new(Vec::new())));
        assert!(matches!(
            a.decide(
                "Bash",
                ToolMeta {
                    read_only: false,
                    idempotent: false
                },
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn the_bash_preview_is_the_command_itself() {
        let p = preview("Bash", &serde_json::json!({ "command": "rm -rf build/" }));
        assert!(p.contains("rm -rf build/"), "{p}");
    }

    #[test]
    fn the_edit_preview_shows_both_sides_of_the_change() {
        let p = preview(
            "Edit",
            &serde_json::json!({
                "file_path": "src/auth.rs",
                "old_string": "let user = legacy(req);",
                "new_string": "let user = session::current(req)?;",
            }),
        );
        assert!(p.contains("src/auth.rs"), "{p}");
        assert!(p.contains("- let user = legacy(req);"), "{p}");
        assert!(p.contains("+ let user = session::current(req)?;"), "{p}");
    }

    #[test]
    fn the_write_preview_states_the_size_rather_than_the_bytes() {
        let p = preview(
            "Write",
            &serde_json::json!({ "file_path": "a.txt", "content": "one\ntwo\n" }),
        );
        assert!(p.contains("a.txt") && p.contains("8 bytes") && p.contains("2 lines"), "{p}");
    }

    #[test]
    fn a_huge_edit_is_cut_rather_than_flooding_the_terminal() {
        let big = "line\n".repeat(500);
        let p = preview(
            "Edit",
            &serde_json::json!({ "file_path": "a", "old_string": big, "new_string": "x" }),
        );
        assert!(p.lines().count() < DIFF_LINES + 5, "{}", p.lines().count());
        assert!(p.contains('…'));
    }
}

// endregion: Tests
