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
//! 2. **`ToolMeta::reaches_network` decides whether bytes may leave this
//!    machine, and it is asked *before* and *separately from* the write
//!    question.** Two axes, because they are two risks: writing is something
//!    the model does deliberately, and egress is how a prompt-injected page
//!    turns a read tool into an exfiltration channel. A tool that reads an
//!    attacker's page and then "searches" for the contents of a `.env` has
//!    written nothing and passes every check in rule 3.
//!
//!    **The grant is per host and lasts the session.** The first fetch to
//!    `docs.rs` asks; every later fetch to `docs.rs` in this process does not;
//!    a fetch to somewhere else asks again. A prompt per fetch would be the
//!    click-through trainer this file exists to avoid, and flipping the web
//!    tools to `read_only: false` — which was the obvious fix — is the same
//!    mistake wearing the other axis' clothes: it would fire on every page
//!    read and cost the prompt on `Write` as well.
//!
//!    **The gate never parses a tool's arguments to find the host.** It calls
//!    [`emma_tool_api::Tool::network_target`] and the tool answers. Twice
//!    before, this loop acquired knowledge of one specific tool's shape, and
//!    both times "adding a tool is a crate plus one registry line" quietly
//!    stopped being true.
//! 3. **`ToolMeta::read_only` decides whether to ask about local damage.**
//!    Read, Glob and Grep run silently; Write, Edit and Bash ask. Prompting for
//!    reads is how an agent becomes unusable in under a minute, and an unusable
//!    gate gets turned off.
//! 4. **The prompt shows what will actually happen** — the command, the diff,
//!    the path and size, the host and the URL or query. A prompt the user
//!    cannot evaluate trains them to press `y`, which is worse than no prompt:
//!    it manufactures consent and leaves a record saying they agreed.
//!
//! **What rule 2 does not cover, and none of it is an oversight.** It gates
//! where bytes go and nothing else. It says nothing about *what comes back*: an
//! approved host is a destination the user chose, not a source they trust, and
//! the page that arrives is still attacker-controlled text entering the model's
//! context. It does not cover a tool that reaches the network incidentally —
//! `Bash` can `curl`, declares `reaches_network: false`, and is gated by rule 3
//! showing a human the command itself, which is more information than a host
//! name. And it enforces nothing: like `read_only` it is a declaration, kept
//! honest by the tool crates' own tests, not by this file.
//!
//! **What is deliberately absent.** There is no persistent always-allow, on
//! either axis. "Yes, and stop asking for this tool" and "yes, and stop asking
//! for this host" both live in a `HashSet` on this struct and die with the
//! process — a permission the user cannot see is a permission they have
//! forgotten they granted, and the place they would not see it is a config file
//! written six weeks ago. The session scope is the longest scope a permission
//! may have, and there is no file anywhere in Emma that lengthens it.
//!
//! **The bypass.** `--dangerously-skip-permissions` exists because scripting
//! exists. It cannot be set from configuration or the environment, it is
//! announced loudly at startup by the caller, and every call it waves through
//! is still written to the session log. The short `--yes` spelling is accepted
//! only alongside `-p`, so the interactive path cannot reach the bypass without
//! typing the whole word.

use std::collections::HashSet;

use emma_tool_api::{NetworkTarget, Tool, ToolMeta};
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
/// `ToolMeta::read_only` answers "can this change anything?". The task
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
/// The missing thing is an axis on `ToolMeta` for the *scope* of a write, not
/// merely its existence. That belongs in `tool-api`. **When it lands, delete
/// this list** — the tools will classify themselves and there will be nothing
/// here left to do.
///
/// `ToolMeta` has since gained a second axis, and it is **not that one**:
/// `reaches_network` splits egress out of `read_only`, which does nothing for
/// the two tools named below. This list still has to go by hand.
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

/// Which of the two questions is being asked, because the two grants a `y`
/// produces are not the same grant.
///
/// [`Question::Tool`] widens to every later call to one tool. [`Question::Network`]
/// widens to every later call to one *host*, by any tool. Neither can stand in
/// for the other, and the enum exists so the wording on screen cannot drift
/// from which one the answer is filed under.
#[derive(Debug, Clone, Copy)]
enum Question<'a> {
    Tool,
    /// A `y` here already covers this host for the rest of the session, so `a`
    /// is accepted and means exactly the same thing. There is no wider network
    /// grant on offer — "always, for any host" is the permission this file
    /// declines to have.
    Network(&'a str),
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
// `decide` is the file. Its arms are in a deliberate order — the bypass,
// egress, read-only, the exemption, the session allowance, then unattended —
// and every path that does not allow returns a sentence the model can act on.
//
// `egress` is a separate function rather than another arm because it is a
// separate question with its own grant, its own scope and its own prompt. A
// tool can pass it and still be refused below; nothing can skip it.
// ---------------------------------------------------------------------------

pub struct Approvals {
    gate: Gate,
    asker: Asker,
    session_allowed: Mutex<HashSet<String>>,
    /// Hosts a human has allowed for the rest of this process.
    ///
    /// Separate from `session_allowed` because the two grants are different
    /// shapes and must not be able to stand in for each other: a tool
    /// allowance covers one tool reaching anywhere, and a host allowance
    /// covers one host reached by anything. Keying both on one set would mean
    /// approving `WebFetch` once approved every host it might ever be pointed
    /// at, which is the grant this design refuses to offer.
    hosts_allowed: Mutex<HashSet<String>>,
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
            hosts_allowed: Mutex::new(HashSet::new()),
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
    /// Takes the `Tool` rather than a name and a bool so both axes and the
    /// destination are read from the tool that is about to run. A caller
    /// passing its own idea of whether something writes, or its own idea of
    /// where something is going, is the gate protecting a claim instead of a
    /// fact.
    ///
    /// This is also the only place `network_target` is called, and it is called
    /// on the tool rather than computed from `args`. That asymmetry is the
    /// point: this file knows that *some* tools reach *some* host, and knows
    /// nothing about how any of them spell it.
    pub async fn request(&self, tool: &dyn Tool, args: &Value, term: &Term) -> Verdict {
        let target = tool.network_target(args);
        let verdict = self
            .decide(tool.name(), tool.meta(), target, args, term)
            .await;
        self.seen
            .lock()
            .await
            .push((tool.name().to_string(), verdict.clone()));
        verdict
    }

    async fn decide(
        &self,
        name: &str,
        meta: ToolMeta,
        target: Option<NetworkTarget>,
        args: &Value,
        term: &Term,
    ) -> Verdict {
        // The bypass first, because it waves through both questions below and
        // reading it once is one chance to get it wrong instead of two.
        if self.gate == Gate::SkipAll {
            return Verdict::Allow;
        }
        // Egress before the local question, and independent of it: a tool can
        // be read-only — genuinely, honestly read-only — and still be the way
        // something leaves this machine. Every arm below this line assumes the
        // network question has already been answered.
        if let deny @ Verdict::Deny(_) = self.egress(name, meta, target, term).await {
            return deny;
        }
        if meta.read_only {
            return Verdict::Allow;
        }
        // The named hole. See `EXEMPT`.
        if EXEMPT.contains(&name) {
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
        match self.ask(name, term, Question::Tool).await {
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

    /// Rule 2: may bytes leave this machine, for this host, at all.
    ///
    /// Returns `Allow` for every tool that does not declare egress, so the
    /// caller can run it unconditionally and the network question cannot be
    /// skipped by an arm added above it later.
    async fn egress(
        &self,
        name: &str,
        meta: ToolMeta,
        target: Option<NetworkTarget>,
        term: &Term,
    ) -> Verdict {
        if !meta.reaches_network {
            return Verdict::Allow;
        }
        let Some(target) = target else {
            // Fail closed. A tool that declares egress and cannot say where is
            // asking for a grant nobody can write down, and the alternative
            // reading — "no target, so nothing to gate" — hands silent network
            // access to whichever tool forgets to implement one method.
            return Verdict::Deny(format!(
                "{name} reaches the network but did not name the host this call would \
                 contact, so there was nothing for the user to approve and it was not run. \
                 That is a defect in {name}; it is not something to work around from here."
            ));
        };
        if self.hosts_allowed.lock().await.contains(&target.host) {
            return Verdict::Allow;
        }
        if self.gate == Gate::Unattended {
            return Verdict::Deny(format!(
                "{name} would send a request to {} and this is a non-interactive run (-p), \
                 so there is nobody to approve that host. It was not run. Work from what is \
                 already on this machine, or tell the user which host needs approving and \
                 stop.",
                target.host
            ));
        }

        term.prompt_header(name, &network_preview(&target));
        match self.ask(name, term, Question::Network(&target.host)).await {
            // `a` grants no more here than `y` does — see `Question::Network`.
            Some(Answer::Yes | Answer::AlwaysThisTool) => {
                self.hosts_allowed.lock().await.insert(target.host.clone());
                term.note(&format!(
                    "{} is approved for the rest of this session (this process only)",
                    target.host
                ));
                Verdict::Allow
            }
            Some(Answer::No) => Verdict::Deny(format!(
                "The user declined to let {name} contact {}. It was not run. Do not retry it \
                 unchanged and do not try a different host to reach the same content — ask \
                 what they would prefer.",
                target.host
            )),
            // End of input, or a scripted run that ran out of answers.
            None => Verdict::Deny(format!(
                "No approval was given for {name} to contact {}, so it was not run.",
                target.host
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
            Asker::Terminal(lines) => {
                let mut lines = lines.lock().await;
                // Same rule as the gate, for the same reason. A `y` left over
                // from an approval — or from a turn that aborted before it was
                // consumed — became a goal on the first real run, and Emma
                // dutifully spent a budget working on it.
                lines.drain();
                lines.next().await
            }
            Asker::Scripted(_) => None,
        }
    }

    async fn ask(&self, name: &str, term: &Term, question: Question<'_>) -> Option<Answer> {
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
                // Anything typed before the question existed cannot be an
                // answer to it. Dropped here rather than consumed, because the
                // line most likely to be waiting is a `y` aimed at the previous
                // question — and approving an unseen command is the failure
                // this whole file exists to prevent.
                let stale = lines.drain();
                if stale > 0 {
                    term.note(&format!(
                        "ignoring {stale} line(s) typed before this question — answer it below"
                    ));
                }
                loop {
                    match question {
                        Question::Tool => term.prompt_question(name),
                        Question::Network(host) => term.prompt_network_question(host),
                    }
                    let line = lines.next().await;
                    // Before anything else is printed, and on both arms. The
                    // terminal echoed the user's return itself; nothing in this
                    // process can see that happen, so the input box has to be
                    // told, or every repaint after this one erases a row too
                    // high and walks up the screen. See `Term::prompt_answered`.
                    term.prompt_answered(line.as_deref());
                    let line = line?;
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

/// What the human is shown before anything leaves the machine.
///
/// Both lines are load-bearing and neither is enough alone. The host is what is
/// being granted, and granted for the rest of the session, so it has to be the
/// thing the eye lands on. The detail is the errand — the URL, the query — and
/// it is the half that distinguishes the fetch the user asked for from the
/// fetch a page asked for. A prompt with only the host cannot tell a search for
/// a crate name from a search for the contents of a file.
///
/// Deliberately not a match on the tool name: unlike [`preview`], this needs no
/// per-tool arm, because the tool already composed the only part that varies.
pub fn network_preview(target: &NetworkTarget) -> String {
    format!("reach {}\n  {}", target.host, target.detail)
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
// the user's source tree. The network tests are written about a tool that is
// `read_only: true`, because a gate that only asked the write question would
// pass every one of them by running the call silently.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A local reader: changes nothing, reaches nothing.
    const LOCAL_READ: ToolMeta = ToolMeta {
        read_only: true,
        reaches_network: false,
        idempotent: true,
    };
    /// A writer: the shape `Write`, `Edit` and `Bash` have.
    const WRITES: ToolMeta = ToolMeta {
        read_only: false,
        reaches_network: false,
        idempotent: false,
    };
    /// The shape `WebFetch` and `WebSearch` have, and the whole reason for the
    /// second axis: honestly read-only, and still how bytes leave.
    const REACHES: ToolMeta = ToolMeta {
        read_only: true,
        reaches_network: true,
        idempotent: true,
    };

    fn target(host: &str) -> Option<NetworkTarget> {
        Some(NetworkTarget::new(host, format!("read https://{host}/x")))
    }

    #[tokio::test]
    async fn a_read_only_tool_is_never_asked_about_even_unattended() {
        let a = Approvals::unattended();
        assert_eq!(
            a.decide("Read", LOCAL_READ, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Allow
        );
    }

    #[tokio::test]
    async fn unattended_denies_a_writer_and_says_why() {
        let a = Approvals::unattended();
        match a
            .decide("Bash", WRITES, None, &Value::Null, &Term::silent())
            .await
        {
            Verdict::Deny(why) => assert!(why.contains("-p"), "{why}"),
            Verdict::Allow => panic!("an unattended run approved a shell command"),
        }
    }

    #[tokio::test]
    async fn always_is_scoped_to_the_tool_and_to_the_process() {
        let a = Approvals::new(
            Gate::Ask,
            Asker::Scripted(Mutex::new(vec![Answer::AlwaysThisTool])),
        );
        assert_eq!(
            a.decide("Write", WRITES, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Allow
        );
        // The second Write needs no answer — the allowance covers it.
        assert_eq!(
            a.decide("Write", WRITES, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Allow
        );
        // …and covers nothing else. The queue is empty, so a different tool
        // gets the no-answer denial rather than riding on Write's allowance.
        assert!(matches!(
            a.decide("Bash", WRITES, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Deny(_)
        ));
    }

    #[tokio::test]
    async fn the_exemption_covers_the_task_writers_and_nothing_that_touches_the_tree() {
        let a = Approvals::unattended();
        let writes = WRITES;
        for exempt in EXEMPT {
            assert_eq!(
                a.decide(exempt, writes, None, &Value::Null, &Term::silent())
                    .await,
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
                    a.decide(gated, writes, None, &Value::Null, &Term::silent())
                        .await,
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
            a.decide("Bash", WRITES, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Deny(_)
        ));
    }

    // -----------------------------------------------------------------------
    // Rule 2: egress
    //
    // Every test below runs a `read_only: true` tool, so a gate that consulted
    // only the write axis would allow all of them silently. That is the
    // regression these are here to catch.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn the_first_call_to_a_host_asks_and_later_ones_do_not() {
        // One scripted `Yes`, three calls. If the grant were per call rather
        // than per host, calls two and three would find an empty queue and be
        // denied — which is the same failure as prompting three times, seen
        // from the test side.
        let a = Approvals::new(Gate::Ask, Asker::Scripted(Mutex::new(vec![Answer::Yes])));
        for attempt in 1..=3 {
            assert_eq!(
                a.decide(
                    "WebFetch",
                    REACHES,
                    target("docs.rs"),
                    &Value::Null,
                    &Term::silent()
                )
                .await,
                Verdict::Allow,
                "fetch {attempt} to an already-approved host was not allowed"
            );
        }
    }

    #[tokio::test]
    async fn a_second_host_is_a_second_question() {
        // The other half, and the half that makes the grant worth having: the
        // queue has exactly one answer, so if approving `docs.rs` also
        // approved everything else this would come back `Allow`.
        let a = Approvals::new(Gate::Ask, Asker::Scripted(Mutex::new(vec![Answer::Yes])));
        assert_eq!(
            a.decide(
                "WebFetch",
                REACHES,
                target("docs.rs"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Allow
        );
        match a
            .decide(
                "WebFetch",
                REACHES,
                target("evil.example"),
                &Value::Null,
                &Term::silent(),
            )
            .await
        {
            Verdict::Deny(why) => assert!(why.contains("evil.example"), "{why}"),
            Verdict::Allow => panic!("a grant for one host covered another"),
        }
    }

    #[tokio::test]
    async fn a_grant_is_the_host_and_not_the_tool() {
        // A host approved for one tool is approved for the next, and approving
        // a tool's first host does not approve its second. Both directions in
        // one test because the failure is a single `HashSet` doing both jobs.
        let a = Approvals::new(Gate::Ask, Asker::Scripted(Mutex::new(vec![Answer::Yes])));
        assert_eq!(
            a.decide(
                "WebSearch",
                REACHES,
                target("docs.rs"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Allow
        );
        assert_eq!(
            a.decide(
                "WebFetch",
                REACHES,
                target("docs.rs"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Allow,
            "a host approved once had to be approved again for a second tool"
        );
    }

    #[tokio::test]
    async fn unattended_denies_egress_and_names_the_host() {
        // Same rule as a writer under `-p`, and the message has to carry both
        // facts: which host, and that the reason nobody approved it is that
        // there is nobody there.
        let a = Approvals::unattended();
        match a
            .decide(
                "WebFetch",
                REACHES,
                target("docs.rs"),
                &Value::Null,
                &Term::silent(),
            )
            .await
        {
            Verdict::Deny(why) => {
                assert!(why.contains("docs.rs"), "{why}");
                assert!(why.contains("-p"), "the model was not told why: {why}");
            }
            Verdict::Allow => panic!("an unattended run reached the network unasked"),
        }
    }

    #[tokio::test]
    async fn the_bypass_waves_egress_through_like_everything_else() {
        // `--dangerously-skip-permissions` is one decision, not one per axis.
        // A bypass that stopped covering a new axis would be a bypass that
        // silently stopped working for scripts.
        let a = Approvals::new(Gate::SkipAll, Asker::Scripted(Mutex::new(Vec::new())));
        assert_eq!(
            a.decide(
                "WebFetch",
                REACHES,
                target("docs.rs"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Allow
        );
    }

    #[tokio::test]
    async fn a_tool_that_declares_egress_and_names_no_host_is_denied() {
        // Fail closed. The opposite reading — "no target, nothing to gate" —
        // hands silent network access to whichever tool forgets the method.
        let a = Approvals::new(
            Gate::Ask,
            Asker::Scripted(Mutex::new(vec![Answer::Yes, Answer::Yes])),
        );
        match a
            .decide("Mystery", REACHES, None, &Value::Null, &Term::silent())
            .await
        {
            Verdict::Deny(why) => assert!(why.contains("did not name the host"), "{why}"),
            Verdict::Allow => panic!("a tool reached an unnamed host"),
        }
    }

    #[tokio::test]
    async fn declining_a_host_denies_and_tells_the_model_not_to_route_around_it() {
        let a = Approvals::new(Gate::Ask, Asker::Scripted(Mutex::new(vec![Answer::No])));
        match a
            .decide(
                "WebFetch",
                REACHES,
                target("docs.rs"),
                &Value::Null,
                &Term::silent(),
            )
            .await
        {
            Verdict::Deny(why) => {
                assert!(why.contains("docs.rs"), "{why}");
                assert!(why.contains("different host"), "{why}");
            }
            Verdict::Allow => panic!("a declined host was contacted"),
        }
    }

    #[tokio::test]
    async fn a_tool_that_both_writes_and_reaches_answers_for_both() {
        // Nothing has this shape today. The test exists because the two axes
        // are checked in sequence, and a `return Allow` in the first arm would
        // make the second unreachable — which nothing else here would notice.
        let both = ToolMeta {
            read_only: false,
            reaches_network: true,
            idempotent: false,
        };
        // One `Yes` for the host, then the queue is empty for the write
        // question, so the call is still denied.
        let a = Approvals::new(Gate::Ask, Asker::Scripted(Mutex::new(vec![Answer::Yes])));
        assert!(matches!(
            a.decide(
                "Uploader",
                both,
                target("docs.rs"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn the_network_prompt_shows_the_host_and_the_errand() {
        // The rule that a prompt the user cannot evaluate manufactures
        // consent, applied to this prompt. The host alone cannot distinguish a
        // search for a crate name from a search for the contents of a file.
        let p = network_preview(&NetworkTarget::new(
            "api.search.brave.com",
            "search for: contents of .env",
        ));
        assert!(p.contains("api.search.brave.com"), "{p}");
        assert!(p.contains("contents of .env"), "{p}");
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
        assert!(
            p.contains("a.txt") && p.contains("8 bytes") && p.contains("2 lines"),
            "{p}"
        );
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
