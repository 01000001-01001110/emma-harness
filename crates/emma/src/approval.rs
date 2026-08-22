//! The approval gate.
//!
//! A tool surface that is read-only by construction bounds the worst a rogue
//! turn can do to reading something it was entitled to read, and needs no gate
//! to achieve it. Emma writes files and runs commands, which inverts that
//! property, and this file is the thing standing in its place.
//!
//! **The precedence order, in one place, because the order is the design.**
//!
//! Read top to bottom; the first line that answers is the answer, and nothing
//! below it gets a say.
//!
//! ```text
//!   1. a PreToolUse hook denial          policy, checked in the loop
//!   2. a `deny` rule                     policy, written in a file
//!   3. --dangerously-skip-permissions    the bypass
//!   4. an `ask` rule                     forces the question back
//!   5. an `allow` rule                   the persisted grant
//!   6. read-only, or EXEMPT              no question to ask
//!   7. a session grant                   this process, from a `y`/`a`
//!   8. ask the human
//! ```
//!
//! Rules 2, 4 and 5 are new and live in [`crate::permissions`]; that file holds
//! the syntax and the matcher, this one holds where they are consulted. Two
//! things about their placement are worth stating rather than deducing:
//!
//! - **`deny` sits above the bypass.** `--dangerously-skip-permissions` used to
//!   be the first thing read here and is now the third. A deny rule is something
//!   the operator wrote down and can see; the bypass is a flag somebody typed to
//!   get through a script. When they disagree, the written one wins — the same
//!   ruling as rule 1, and the same one Claude Code makes ("if a tool is denied
//!   at any level, no other level can allow it").
//! - **`ask` sits above `allow` and above the session grant**, so it is a real
//!   escape hatch: "I allow this tool generally, and I want to be asked about it
//!   today" is expressible, and cannot be undone by a `y` typed an hour ago.
//!
//! **The rules the two axes follow, unchanged.**
//!
//! 1. **A `PreToolUse` hook that denies wins.** It is checked before any human
//!    is asked, and no answer overrides it — not a `y`, not a session
//!    allowance, not `--dangerously-skip-permissions`, and not an `allow` rule
//!    in a settings file. A hook is policy the operator wrote down; the prompt
//!    is convenience for the person sitting there. If a human could wave a hook
//!    through, the hook would be advice. (The check itself lives in the loop,
//!    which is where the hook runner is. What is enforced here is that nothing
//!    in this file can undo it — and the loop `return`s on a denial before
//!    `Approvals::request` is reached, so it is structure and not a convention.)
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
//!    **The `Write` prompt was failing this rule, by this rule's own words.** It
//!    said `write src/auth.rs — 4,102 bytes, 118 lines` and stopped: a path, a
//!    size, and not one word about the change. There is no answer a reader can
//!    give to that except the one this paragraph is written against. It now
//!    carries the diff, computed against the file on disk, and so does `Edit`.
//!    Both go through [`crate::term::diff`], which is also where every case that
//!    *cannot* honestly be diffed — a binary file, one too large to read, a
//!    changed region too large to line up — is turned into a sentence saying so
//!    rather than into something plausible. A fabricated diff would be this rule
//!    inverted: a prompt the user can evaluate, wrongly.
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
//! **What used to be deliberately absent, and what replaced it.** This file
//! carried a section arguing that there must be no persistent always-allow on
//! either axis: "a permission the user cannot see is a permission they have
//! forgotten they granted, and the place they would not see it is a config file
//! written six weeks ago." The hazard was real and the conclusion was wrong,
//! measured against one goal that cost five prompts and re-asked all five on the
//! next run. What the argument actually demands is not the absence of a
//! persistent grant but its **visibility**, and that is what
//! [`crate::permissions`] is built to provide: a grant is written only when the
//! user picks the answer that says so, the exact rule is on screen before it is
//! written, it lands in a JSON file they can read and delete, and `emma config
//! check` lists every rule with the file it came from.
//!
//! The three unpersisted answers are still here and still mean what they meant.
//! `y` grants one call — or, on the network question, one host for this process.
//! `a` grants one tool for this process. Neither writes anything, and the
//! `HashSet`s they fill still die with it.
//!
//! **The bypass.** `--dangerously-skip-permissions` exists because scripting
//! exists. It cannot be set from configuration or the environment, it is
//! announced loudly at startup by the caller, and every call it waves through
//! is still written to the session log. The short `--yes` spelling is accepted
//! only alongside `-p`, so the interactive path cannot reach the bypass without
//! typing the whole word.

use std::collections::HashSet;
use std::path::PathBuf;

use emma_tool_api::{NetworkTarget, Tool, ToolMeta};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::permissions::{Decision, Rule, Rules};
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
///
/// The two `Remember` answers are the only ones that write to disk, and they are
/// separate variants rather than one with a width flag so that "which rule did
/// that keystroke grant" is answered by the type and not by an argument that can
/// be passed wrong. Their *content* is not here on purpose: the rule is composed
/// by the gate, from the tool and the host it is holding, and shown to the user
/// in the question. An `Answer` carrying its own rule string would be a second
/// place for the granted rule to be decided, one of them out of sight of the
/// prompt that displayed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    /// Yes, and stop asking for this tool — for this process only.
    AlwaysThisTool,
    /// Yes, and write down the **narrow** rule the prompt showed: the host on
    /// the network question, the tool on the local one.
    RememberNarrow,
    /// Yes, and write down the **tool-wide** rule the prompt showed — every call
    /// to this tool, any host. Offered as a distinct keystroke rather than
    /// inferred, because it is a materially larger grant than the narrow one and
    /// nobody should arrive at it by pressing the same key.
    RememberWide,
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
    /// is accepted and means exactly the same thing.
    Network(&'a str),
}

/// The rules a keystroke at this prompt would write, if any.
///
/// Built by the gate and handed to both the terminal and the answer loop, so the
/// text on screen and the text written to the file are the same `String` and
/// cannot drift. The whole point of the feature is that nobody discovers later
/// that they granted more than they meant, and two independently formatted
/// copies of a rule is exactly how that happens.
///
/// `None` in either slot means the keystroke is neither offered nor accepted.
/// Both are `None` when there is nowhere to write — an unattended run, or a
/// harness whose directory could not be resolved — because offering a persistent
/// grant that silently does not persist is worse than not offering one.
#[derive(Debug, Default, Clone)]
struct Offers {
    /// The host rule on the network question; the bare tool rule on the local
    /// one, where "narrow" and "tool-wide" are the same grant.
    narrow: Option<Rule>,
    /// The bare tool rule. Offered on the network question only, where it is
    /// genuinely wider than `narrow` — this is "all web searches in the
    /// directory", asked for in those words.
    wide: Option<Rule>,
}

impl Offers {
    fn text(rule: &Option<Rule>) -> Option<String> {
        rule.as_ref().map(Rule::to_string)
    }
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
    /// Whether a human could have typed ahead of a prompt.
    ///
    /// True for a keyboard, false for a pipe. The drain at the goal prompt
    /// exists so a stale `y` from an approval cannot become a goal — a real
    /// property, and one that means nothing on a pipe, where every line was
    /// supplied deliberately and all of it arrives before anything is read.
    /// Draining there discarded the input and then explained the loss in terms
    /// of a keyboard that does not exist, so `echo "goal" | emma` could never
    /// run its goal.
    ///
    /// A value rather than a call to `stdin().is_terminal()` at the point of
    /// use: under `cargo test` stdin is never a terminal, so reading the
    /// process there would have silently disabled the drain in every test that
    /// covers it — which is exactly what it did, and what the suite caught.
    type_ahead_possible: bool,
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
    /// The rules from disk, plus anything this run has added to it.
    ///
    /// Behind a `Mutex` because a remembered grant is adopted into the live set
    /// the moment it is written: without that, "yes, and remember this" would
    /// prompt again on the very next call and the user would reasonably conclude
    /// the feature does not work.
    rules: Mutex<Rules>,
    /// Where a remembered rule is written, and `None` when there is nowhere.
    ///
    /// One field decides both whether the keystroke is offered and whether it
    /// would persist, so those two cannot disagree — an option on screen that
    /// silently does not persist is worse than no option.
    file: Option<PathBuf>,
}

impl Approvals {
    pub fn new(gate: Gate, asker: Asker) -> Self {
        Self {
            gate,
            asker,
            session_allowed: Mutex::new(HashSet::new()),
            hosts_allowed: Mutex::new(HashSet::new()),
            seen: Mutex::new(Vec::new()),
            rules: Mutex::new(Rules::default()),
            file: None,
            // Defaults to "a human might be typing", which is the safe side:
            // the drain protects against a stale answer becoming a goal, and
            // running it when it was not needed costs a message, while skipping
            // it when it was needed costs a budget spent on the wrong thing.
            type_ahead_possible: true,
        }
    }

    /// Say that nothing can be typed ahead — stdin is a pipe, not a keyboard.
    ///
    /// Set by `main` from `stdin().is_terminal()`, once, rather than read at
    /// each prompt: under `cargo test` stdin is never a terminal, so a check at
    /// the point of use disables the drain in every test that covers it.
    pub fn piped(mut self) -> Self {
        self.type_ahead_possible = false;
        self
    }

    /// The rules this run operates under, and the file a remembered one goes in.
    ///
    /// Separate from [`Approvals::new`] rather than two more parameters on it,
    /// because every existing caller and every existing test builds a gate with
    /// no rules and that must keep meaning "ask about everything". A rule set
    /// that arrived by default is how a gate acquires a permission nobody chose.
    pub fn with_rules(mut self, rules: Rules, file: Option<PathBuf>) -> Self {
        self.rules = Mutex::new(rules);
        self.file = file;
        self
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
        // Only bare `Tool` rules answer this question; a `Tool(domain:…)` grant
        // is about a destination, and reading it as permission to run the tool
        // would let a narrow grant answer a question nobody asked it.
        let rule = self.rules.lock().await.for_tool(name);

        // A `deny` rule first, above the bypass. See the precedence block at the
        // top of the file: written policy outranks a flag somebody typed.
        if rule == Some(Decision::Deny) {
            return Verdict::Deny(format!(
                "A `deny` permission rule forbids {name}. It was not run, and no answer at a \
                 prompt can change that — it is written down in a settings file. Do not retry \
                 it; tell the user which rule is in the way if they need to know."
            ));
        }
        // Egress before the local question, and independent of it: a tool can
        // be read-only — genuinely, honestly read-only — and still be the way
        // something leaves this machine. Every arm below this line assumes the
        // network question has already been answered.
        if let deny @ Verdict::Deny(_) = self.egress(name, meta, target, term).await {
            return deny;
        }
        if self.gate == Gate::SkipAll {
            return Verdict::Allow;
        }
        // An `ask` rule puts the question back, over an `allow` rule, over
        // `read_only`, over the exemption and over a session grant. That is the
        // whole value of having a third list: "allowed in general, ask me today"
        // has to be expressible, and it cannot be undone by a `y` typed an hour
        // ago or it is not a rule, it is a preference.
        let forced = rule == Some(Decision::Ask);
        if !forced {
            if rule == Some(Decision::Allow) {
                return Verdict::Allow;
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
        }
        if self.gate == Gate::Unattended {
            return Verdict::Deny(format!(
                "{name} needs approval and this is a non-interactive run (-p), so there is \
                 nobody to ask. It was not run. Use a read-only tool, or tell the user what \
                 needs approving and stop."
            ));
        }

        // On this question the narrow rule and the tool-wide one are the same
        // grant, so there is one remember key rather than two that do the same
        // thing. And nothing is offered when an `ask` rule forced the prompt: the
        // `allow` it would write is outranked by that very rule, so the keystroke
        // would write a grant that does nothing.
        let offers = match (forced, &self.file) {
            (false, Some(_)) => Offers {
                narrow: Some(Rule::every_call(name)),
                wide: None,
            },
            _ => Offers::default(),
        };

        term.prompt_header(name, &preview(name, args));
        // `ask` loops on unreadable input; by the time it answers, the answer is
        // one of the offered ones.
        match self.ask(name, term, Question::Tool, &offers).await {
            Some(Answer::Yes) => Verdict::Allow,
            Some(Answer::AlwaysThisTool) => {
                self.session_allowed.lock().await.insert(name.to_string());
                term.note(&format!(
                    "{name} is approved for the rest of this session (this process only)"
                ));
                Verdict::Allow
            }
            Some(Answer::RememberNarrow | Answer::RememberWide) => {
                // Both keys mean the same rule here, and the fallback covers the
                // scripted asker, which can hand back an answer the terminal
                // would not have offered.
                let rule = offers
                    .narrow
                    .clone()
                    .unwrap_or_else(|| Rule::every_call(name));
                self.keep(rule, term).await;
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

    /// Write a rule down, adopt it for the rest of this run, and say so.
    ///
    /// **A failed write is a warning and not a denial.** The user answered the
    /// question; refusing the call because the *bookkeeping* failed would punish
    /// them for a permissions problem on a file. What they lose is the
    /// persistence, and they are told exactly that, with the reason — including
    /// the case that matters most, a settings file this run declined to touch
    /// because it could not parse it.
    async fn keep(&self, rule: Rule, term: &Term) {
        match &self.file {
            Some(file) => match crate::permissions::remember(file, &rule) {
                // The rule and the file, both, every time. A grant whose text is
                // on screen and whose location is not is a grant the user cannot
                // go and revoke.
                Ok(wrote) => term.note(&format!(
                    "{} `{rule}` in {} — Emma will not ask about this again",
                    if wrote { "saved" } else { "already had" },
                    file.display()
                )),
                Err(e) => term.warn(&format!(
                    "`{rule}` was allowed for this session but NOT saved: {e:#}"
                )),
            },
            // Not reachable from the terminal, which offers the key only when
            // there is a file. A scripted run can get here, and saying so is
            // better than pretending something was written.
            None => term.warn(&format!(
                "`{rule}` was not saved: this run has nowhere to write permission rules"
            )),
        }
        // **Adopted on every path, including both failures.** The user answered
        // the question; the file is how the answer survives the process, not how
        // it takes effect. A `keep` that returned early on a write error would
        // re-ask for the rest of the run — punishing them a second time for a
        // problem with a file.
        self.rules.lock().await.adopt(rule);
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
        // Bare `Tool` rules and this tool's `domain:` rules both answer here.
        let rule = self.rules.lock().await.for_egress(name, &target.host);
        if rule == Some(Decision::Deny) {
            return Verdict::Deny(format!(
                "A `deny` permission rule forbids {name} from contacting {}. It was not run, \
                 no answer at a prompt can change that, and a different host to reach the \
                 same content is not a workaround — it is the thing the rule is about.",
                target.host
            ));
        }
        // The bypass, here as well as in `decide`. Both questions are waved
        // through by one flag; a `deny` rule is what neither of them waves.
        if self.gate == Gate::SkipAll {
            return Verdict::Allow;
        }
        let forced = rule == Some(Decision::Ask);
        if !forced {
            if rule == Some(Decision::Allow) {
                return Verdict::Allow;
            }
            if self.hosts_allowed.lock().await.contains(&target.host) {
                return Verdict::Allow;
            }
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

        // The two grants on offer, and they are deliberately different sizes.
        // The narrow one is the host the user is looking at — the natural answer
        // for a `WebFetch` on `apnews.com`. The wide one is every call to this
        // tool, any host, which is "all web searches in the directory" as it was
        // asked for; it is a separate keystroke because guessing which one
        // somebody meant is how they end up granting more than they read.
        let offers = match (forced, &self.file) {
            (false, Some(_)) => Offers {
                narrow: Some(Rule::domain(name, &target.host)),
                wide: Some(Rule::every_call(name)),
            },
            _ => Offers::default(),
        };

        term.prompt_header(name, &network_preview(&target));
        match self
            .ask(name, term, Question::Network(&target.host), &offers)
            .await
        {
            // `a` grants no more here than `y` does — see `Question::Network`.
            Some(Answer::Yes | Answer::AlwaysThisTool) => {
                self.hosts_allowed.lock().await.insert(target.host.clone());
                term.note(&format!(
                    "{} is approved for the rest of this session (this process only)",
                    target.host
                ));
                Verdict::Allow
            }
            Some(answer @ (Answer::RememberNarrow | Answer::RememberWide)) => {
                let rule = match answer {
                    Answer::RememberWide => offers.wide.clone(),
                    _ => offers.narrow.clone(),
                };
                self.keep(
                    rule.unwrap_or_else(|| Rule::domain(name, &target.host)),
                    term,
                )
                .await;
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
    /// `term` is here only so the drain can say what it dropped.
    ///
    /// **It used to drain silently, and that is a defect this prompt could not
    /// afford.** The approval path already reports "ignoring N line(s)"; the
    /// goal prompt did not, so a `/exit` typed while a goal was still finishing
    /// — or before the prompt existed at all — vanished with nothing on screen.
    /// The symptom is indistinguishable from a command that does not work,
    /// which is exactly what it was reported as.
    pub async fn read_line(&self, term: &Term) -> Option<String> {
        match &self.asker {
            Asker::Terminal(lines) => {
                let mut lines = lines.lock().await;
                // Same rule as the gate, for the same reason. A `y` left over
                // from an approval — or from a turn that aborted before it was
                // consumed — became a goal on the first real run, and Emma
                // dutifully spent a budget working on it.
                // **Only when a keyboard is what is on the other end.** On a
                // pipe there is no such thing as "typed before the prompt":
                // the whole stream was supplied deliberately, all of it
                // arrives before anything is read, and draining it discarded
                // the input and then explained the loss in terms of a keyboard
                // that does not exist. A piped `/exit` printed "ignoring 1
                // line(s)" and hung to EOF, so `echo "goal" | emma` could
                // never run its goal at all.
                //
                // The property the drain protects is unchanged where it
                // applies: a human typing ahead of a prompt still loses it,
                // loudly. A script cannot type ahead — everything it sends is
                // on purpose.
                let stale = if self.type_ahead_possible {
                    lines.drain()
                } else {
                    0
                };
                if stale > 0 {
                    term.note(&format!(
                        "ignoring {stale} line(s) typed before this prompt — nothing reads the \
                         keyboard while a goal runs, so type it again"
                    ));
                }
                lines.next().await
            }
            Asker::Scripted(_) => None,
        }
    }

    /// The grants given by answering `[a]` or `[y]` this session: tools, then
    /// hosts.
    ///
    /// For `/clear`, which keeps them and has to be able to name them. A
    /// receipt that said "grants were kept" without saying which would be the
    /// silence the whole command is written against.
    pub async fn session_grants(&self) -> (Vec<String>, Vec<String>) {
        let mut tools: Vec<String> = self.session_allowed.lock().await.iter().cloned().collect();
        let mut hosts: Vec<String> = self.hosts_allowed.lock().await.iter().cloned().collect();
        // A `HashSet` has no order and a receipt that reshuffles itself between
        // two readings is one nobody trusts.
        tools.sort();
        hosts.sort();
        (tools, hosts)
    }

    async fn ask(
        &self,
        name: &str,
        term: &Term,
        question: Question<'_>,
        offers: &Offers,
    ) -> Option<Answer> {
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
                let (narrow, wide) = (Offers::text(&offers.narrow), Offers::text(&offers.wide));
                loop {
                    // The rule text goes on screen *before* the key that writes
                    // it is pressed, and it is the same string `keep` writes —
                    // one `Rule`, formatted once. Nobody is to discover later
                    // that they granted something other than what they read.
                    match question {
                        Question::Tool => term.prompt_question(name, narrow.as_deref()),
                        Question::Network(host) => {
                            term.prompt_network_question(host, narrow.as_deref(), wide.as_deref())
                        }
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
                        // Guarded on the offer rather than on the question, so a
                        // key that was not shown is a key that does not work. A
                        // prompt with an undocumented answer that writes to disk
                        // is the same trap as a prompt nobody can evaluate.
                        "r" | "remember" if narrow.is_some() => {
                            return Some(Answer::RememberNarrow)
                        }
                        "t" | "trust" if wide.is_some() => return Some(Answer::RememberWide),
                        other => {
                            let mut keys = String::from("y / n / a");
                            if narrow.is_some() {
                                keys.push_str(" / r");
                            }
                            if wide.is_some() {
                                keys.push_str(" / t");
                            }
                            term.note(&format!("`{other}` is not one of {keys}"));
                        }
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
// the change itself under each of them.
//
// **What the `Edit` arm used to say, and why the replacement is the same
// argument rather than a reversal of it.** This file carried a rule that the
// two sides must be shown *verbatim*, never as a computed diff, because `Edit`'s
// arguments are the two sides and a verbatim rendering is the only one that
// cannot disagree with what the tool will do. That premise is intact and is why
// `term::diff` reads nothing for an `Edit`: the same two argument strings go in,
// and what comes out is those strings with the lines they have in common
// written once instead of twice. Nothing is minimised away — every changed line
// is still there under its own sign — so it is not the summary the old rule
// refused. It is the same evidence, arranged so the reader does not have to do
// the diffing that the tool has already decided.
//
// `Write` is the arm that was genuinely failing the file's own standard: it
// named a path and a byte count and withheld the change entirely.
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
        // The two writing tools, and the one thing they used to withhold. See
        // `term::diff` for what each can honestly show and for every case where
        // the answer is a sentence rather than a diff.
        "Write" => {
            let content = s("content");
            let mut out = format!(
                "write {}\n  {} bytes, {} lines",
                s("file_path"),
                content.len(),
                content.lines().count()
            );
            if let Some(change) = crate::term::diff::for_call(tool, args) {
                out.push('\n');
                out.push_str(&change.to_text(crate::term::diff::BUDGET));
            }
            out
        }
        "Edit" => {
            let all = args
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut out = format!(
                "edit {}{}",
                s("file_path"),
                if all { "  (every occurrence)" } else { "" }
            );
            if let Some(change) = crate::term::diff::for_call(tool, args) {
                out.push('\n');
                out.push_str(&change.to_text(crate::term::diff::BUDGET));
            }
            out
        }
        // A delegation is the one call in the surface where the *model* wrote
        // the instructions, so the prompt shows which agent, and the brief
        // itself — capped like the diff is. Without this arm it falls to the
        // pretty-JSON default and the prompt is a blob nobody reads, which is
        // the prompt that manufactures consent.
        crate::delegate::NAME => {
            let mut out = format!("delegate to {}", s("agent"));
            let context = args
                .get("context")
                .and_then(Value::as_array)
                .map(|c| c.len())
                .unwrap_or(0);
            if context > 0 {
                out.push_str(&format!("  ({context} facts handed down)"));
            }
            for line in s("task").lines().take(BRIEF_LINES) {
                out.push_str(&format!("\n  {line}"));
            }
            if s("task").lines().count() > BRIEF_LINES {
                out.push_str("\n  …");
            }
            if !s("deliver").is_empty() {
                out.push_str(&format!("\n  must deliver: {}", s("deliver")));
            }
            out
        }
        _ => serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string()),
    }
}

/// How much of a delegation's brief the prompt shows. The brief is prose a model
/// wrote and can be arbitrarily long; the question is "should this run at all",
/// and the first few lines answer it.
const BRIEF_LINES: usize = 12;

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

    /// The goal prompt drains, and now says so.
    ///
    /// **Written from a real report: `/exit` "stopped working".** Nothing reads
    /// the keyboard while a goal is in flight, so a command typed then is
    /// dropped at the next prompt — correctly, for the reason the whole drain
    /// exists — and this path used to drop it in silence. A user cannot tell a
    /// line that was eaten from a command that does not work, and they will
    /// report the second.
    #[tokio::test]
    async fn a_piped_session_keeps_its_input_instead_of_draining_it() {
        // The other side of the drain. On a pipe there is no such thing as
        // typing ahead: every line was supplied deliberately and all of it
        // arrives before anything reads it. Draining discarded the lot and then
        // explained the loss in terms of a keyboard that does not exist, so
        // `echo "goal" | emma` could never run its goal — reproduced live by
        // two independent audits before this existed.
        let term = Term::recording();
        let a = Approvals::new(
            Gate::Ask,
            Asker::Terminal(crate::term::input::LineSource::scripted(&["/exit", "hello"]).into()),
        )
        .piped();
        assert_eq!(
            a.read_line(&term).await.as_deref(),
            Some("/exit"),
            "a piped line was thrown away"
        );
        let said = term.recorded().join(
            "
",
        );
        assert!(
            !said.contains("ignoring"),
            "it explained a loss that did not happen: {said}"
        );
    }

    #[tokio::test]
    async fn a_line_dropped_at_the_goal_prompt_is_reported_rather_than_vanishing() {
        let term = Term::recording();
        let a = Approvals::new(
            Gate::Ask,
            Asker::Terminal(crate::term::input::LineSource::scripted(&["/exit", "hello"]).into()),
        );
        // The queue is drained, so `read_line` finds nothing left and ends.
        assert_eq!(a.read_line(&term).await, None);
        let said = term.recorded().join("\n");
        assert!(said.contains("ignoring 2 line"), "{said}");
        // …and it says why, because "your input was thrown away" with no reason
        // reads as a bug rather than as the safety property it is.
        assert!(said.contains("while a goal runs"), "{said}");

        // Nothing dropped, nothing said. A false "your input was eaten" is its
        // own defect.
        let term = Term::recording();
        let a = Approvals::new(
            Gate::Ask,
            Asker::Terminal(crate::term::input::LineSource::scripted(&[]).into()),
        );
        assert_eq!(a.read_line(&term).await, None);
        assert!(term.recorded().is_empty(), "{:?}", term.recorded());
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

    // -----------------------------------------------------------------------
    // Rules
    //
    // The precedence order from the top of this file, tested in the direction
    // that costs something when it inverts. `permissions.rs` owns the matcher
    // and its near-misses; what is pinned here is *where the gate consults it*,
    // which is the half that can be broken by moving one `if`.
    // -----------------------------------------------------------------------

    /// A gate carrying rules and no answers at all, so anything reaching the
    /// prompt is denied — which makes "allowed" mean "a rule allowed it" and
    /// nothing else.
    fn ruled(gate: Gate, deny: &[&str], ask: &[&str], allow: &[&str]) -> Approvals {
        let mut entries = Vec::new();
        for (kind, list) in [
            (emma_harness::PermissionKind::Deny, deny),
            (emma_harness::PermissionKind::Ask, ask),
            (emma_harness::PermissionKind::Allow, allow),
        ] {
            for rule in list {
                entries.push(emma_harness::PermissionEntry {
                    rule: (*rule).to_string(),
                    kind,
                    source: std::path::PathBuf::from("settings.local.json"),
                });
            }
        }
        let (rules, notes) = crate::permissions::Rules::parse(&entries);
        assert!(notes.is_empty(), "a test wrote an unusable rule: {notes:?}");
        Approvals::new(gate, Asker::Scripted(Mutex::new(Vec::new()))).with_rules(rules, None)
    }

    #[tokio::test]
    async fn an_allow_rule_is_why_the_second_run_does_not_ask() {
        // The whole feature, from the user's side: five hosts approved once,
        // and the next process does not ask about any of them. The queue is
        // empty, so every `Allow` below is the rule and not a scripted answer.
        let hosts = [
            "api.search.brave.com",
            "www.reuters.com",
            "tech.yahoo.com",
            "openai.com",
            "apnews.com",
        ];
        let rules: Vec<String> = hosts
            .iter()
            .map(|h| format!("WebFetch(domain:{h})"))
            .collect();
        let a = ruled(
            Gate::Ask,
            &[],
            &[],
            &rules.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        for host in hosts {
            assert_eq!(
                a.decide(
                    "WebFetch",
                    REACHES,
                    target(host),
                    &Value::Null,
                    &Term::silent()
                )
                .await,
                Verdict::Allow,
                "{host} was approved in a settings file and asked anyway"
            );
        }
        // …and a sixth host still asks. A persisted grant that widened to
        // everything would be the bug this whole file is arranged against.
        assert!(matches!(
            a.decide(
                "WebFetch",
                REACHES,
                target("evil.example"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Deny(_)
        ));
    }

    #[tokio::test]
    async fn a_deny_rule_beats_an_allow_rule_on_both_axes() {
        // The guarantee. Delete the deny check and both halves go green as
        // `Allow`, which is a settings file whose `deny` list is decoration.
        let a = ruled(Gate::Ask, &["WebFetch"], &[], &["WebFetch(domain:docs.rs)"]);
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
            Verdict::Deny(why) => assert!(why.contains("deny"), "{why}"),
            Verdict::Allow => panic!("an allow rule overrode a deny rule on the network axis"),
        }
        // The same, on the local axis, where the deny is narrow and the allow
        // is the broad one.
        let a = ruled(Gate::Ask, &["Bash"], &[], &["Bash", "Write"]);
        assert!(matches!(
            a.decide("Bash", WRITES, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Deny(_)
        ));
        assert_eq!(
            a.decide("Write", WRITES, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Allow,
            "a deny on one tool swallowed the allow on another"
        );
    }

    #[tokio::test]
    async fn a_deny_rule_beats_the_bypass_flag() {
        // `--dangerously-skip-permissions` used to be the first thing `decide`
        // read. It is now third, behind the hook denial and the deny rule, and
        // this is the test that keeps it there: a written-down refusal is not
        // something a flag on the command line gets to override.
        let a = ruled(Gate::SkipAll, &["Bash"], &[], &[]);
        assert!(matches!(
            a.decide("Bash", WRITES, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Deny(_)
        ));
        // …and on the egress axis, which is a separate `if` and so a separate
        // way to get it wrong.
        let a = ruled(Gate::SkipAll, &["WebFetch(domain:evil.example)"], &[], &[]);
        assert!(matches!(
            a.decide(
                "WebFetch",
                REACHES,
                target("evil.example"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Deny(_)
        ));
        // The bypass still bypasses everything nobody wrote a rule about,
        // which is the half that keeps this from being "the flag stopped
        // working".
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

    // A hook denial outranking every rule in every file is a property of the
    // *loop*, not of this file — `agent.rs` resolves the `PreToolUse` verdict
    // and returns before `Approvals::request` exists to be called, so nothing
    // here can undo it and nothing here can test it either. It is guarded
    // behaviourally, three times, in
    // `tests/permissions.rs::a_hook_denial_outranks_an_allow_rule_that_covers_the_call`
    // and in `tests/loop.rs`. A source-order assertion was written here first
    // and removed: it passed against a mutation that disabled the denial
    // outright, which makes it worse than nothing.

    #[tokio::test]
    async fn an_ask_rule_puts_the_question_back_over_an_allow_and_over_a_grant() {
        // The reason the third list is worth having. Two `Yes` answers are
        // queued: if `ask` did not force the prompt, neither would be consumed
        // and the second call would still be `Allow` — which is the failure
        // this catches.
        let a = Approvals::new(
            Gate::Ask,
            Asker::Scripted(Mutex::new(vec![Answer::AlwaysThisTool])),
        )
        .with_rules(
            crate::permissions::Rules::parse(&[
                emma_harness::PermissionEntry {
                    rule: "Write".into(),
                    kind: emma_harness::PermissionKind::Ask,
                    source: std::path::PathBuf::new(),
                },
                emma_harness::PermissionEntry {
                    rule: "Write".into(),
                    kind: emma_harness::PermissionKind::Allow,
                    source: std::path::PathBuf::new(),
                },
            ])
            .0,
            None,
        );
        assert_eq!(
            a.decide("Write", WRITES, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Allow
        );
        // `a` filled the session grant, and the `ask` rule outranks it, so the
        // second call asks again — and the queue is empty.
        assert!(
            matches!(
                a.decide("Write", WRITES, None, &Value::Null, &Term::silent())
                    .await,
                Verdict::Deny(_)
            ),
            "an `ask` rule was outranked by a session grant"
        );
    }

    #[tokio::test]
    async fn an_ask_rule_reaches_even_a_read_only_tool_and_an_exempt_one() {
        // Both shortcuts that exist to keep the gate usable — `read_only` and
        // the `EXEMPT` list — are below the `ask` rule, because an operator who
        // writes `ask` about a tool has said something more specific than
        // either default.
        let a = ruled(Gate::Ask, &[], &["Read", EXEMPT[0]], &[]);
        assert!(matches!(
            a.decide("Read", LOCAL_READ, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Deny(_)
        ));
        assert!(matches!(
            a.decide(EXEMPT[0], WRITES, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Deny(_)
        ));
    }

    #[tokio::test]
    async fn a_domain_rule_does_not_become_permission_to_run_the_tool() {
        // A grant naming one destination is not a grant to do local damage.
        // Nothing has this shape today; the first tool that both writes and
        // names a host would be the one that pays for getting it wrong.
        let both = ToolMeta {
            read_only: false,
            reaches_network: true,
            idempotent: false,
        };
        let a = ruled(Gate::Ask, &[], &[], &["Uploader(domain:docs.rs)"]);
        assert!(matches!(
            a.decide(
                "Uploader",
                both,
                target("docs.rs"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Deny(_),
        ));
    }

    #[tokio::test]
    async fn remembering_writes_the_rule_shown_and_stops_the_next_prompt() {
        // The round trip the owner asked for, end to end and through the same
        // code path a person drives: one answer, a file on disk, and no second
        // question — for that host and for nothing else.
        let dir = tempfile::tempdir().unwrap();
        let file = crate::permissions::file_for(dir.path());
        let a = Approvals::new(
            Gate::Ask,
            Asker::Scripted(Mutex::new(vec![Answer::RememberNarrow])),
        )
        .with_rules(crate::permissions::Rules::default(), Some(file.clone()));

        assert_eq!(
            a.decide(
                "WebFetch",
                REACHES,
                target("apnews.com"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Allow
        );
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(written.contains("WebFetch(domain:apnews.com)"), "{written}");

        // The queue is empty now, so a second `Allow` can only come from the
        // rule having been adopted into the live set.
        assert_eq!(
            a.decide(
                "WebFetch",
                REACHES,
                target("apnews.com"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Allow,
            "the grant was written to disk and not applied to this run"
        );
        // …and it did not widen. This is the assertion that fails if `remember`
        // ever writes the tool instead of the host.
        assert!(matches!(
            a.decide(
                "WebFetch",
                REACHES,
                target("evil.example"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Deny(_)
        ));
    }

    #[tokio::test]
    async fn trusting_the_tool_is_a_wider_grant_and_a_different_key() {
        // "all web searches in the directory", which is the other half of what
        // was asked for. `RememberWide` writes the bare tool name, and the
        // difference from the test above is the whole reason the two answers
        // are separate keystrokes.
        let dir = tempfile::tempdir().unwrap();
        let file = crate::permissions::file_for(dir.path());
        let a = Approvals::new(
            Gate::Ask,
            Asker::Scripted(Mutex::new(vec![Answer::RememberWide])),
        )
        .with_rules(crate::permissions::Rules::default(), Some(file.clone()));
        assert_eq!(
            a.decide(
                "WebSearch",
                REACHES,
                target("api.search.brave.com"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Allow
        );
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(written.contains("\"WebSearch\""), "{written}");
        assert!(!written.contains("domain"), "{written}");
        // Any host, now — and still only that tool.
        assert_eq!(
            a.decide(
                "WebSearch",
                REACHES,
                target("somewhere.else"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Allow
        );
        assert!(matches!(
            a.decide(
                "WebFetch",
                REACHES,
                target("somewhere.else"),
                &Value::Null,
                &Term::silent()
            )
            .await,
            Verdict::Deny(_)
        ));
    }

    #[tokio::test]
    async fn a_run_with_nowhere_to_write_still_honours_the_answer_for_the_session() {
        // A scripted or unattended run can produce a `Remember` answer that
        // cannot be persisted. The call is allowed and the grant lasts the
        // process: the user answered the question, and failing the call over
        // bookkeeping would punish them for it.
        let a = Approvals::new(
            Gate::Ask,
            Asker::Scripted(Mutex::new(vec![Answer::RememberNarrow])),
        );
        assert_eq!(
            a.decide("Write", WRITES, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Allow
        );
        assert_eq!(
            a.decide("Write", WRITES, None, &Value::Null, &Term::silent())
                .await,
            Verdict::Allow
        );
    }

    #[test]
    fn the_prompt_names_the_rule_it_would_write() {
        // The rule is on screen before the key that writes it is pressed, and
        // it is the same string the file gets. A prompt that said "remember
        // this" and wrote something the user never saw would be the exact
        // failure the module doc is written against.
        let term = Term::silent();
        term.prompt_network_question(
            "apnews.com",
            Some(&Rule::domain("WebFetch", "apnews.com").to_string()),
            Some(&Rule::every_call("WebFetch").to_string()),
        );
        // Constructed the way the gate constructs it, so a change to `Rule`'s
        // rendering shows up here rather than only in the file.
        assert_eq!(
            Rule::domain("WebFetch", "apnews.com").to_string(),
            "WebFetch(domain:apnews.com)"
        );
        assert_eq!(Rule::every_call("WebSearch").to_string(), "WebSearch");
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

    /// The half of an `Edit` prompt that is new: the lines the two sides share
    /// are shown once, as context, instead of once under `-` and again under
    /// `+`. Nothing is minimised away — both changed lines are still on screen —
    /// which is what keeps this inside the rule the module doc states rather
    /// than a summary of the change.
    #[test]
    fn the_edit_preview_no_longer_makes_the_reader_do_the_diffing() {
        let old = "fn handler(req: Req) -> Res {\n    let user = legacy(req);\n    render(user)\n}";
        let new =
            "fn handler(req: Req) -> Res {\n    let user = current(req)?;\n    render(user)\n}";
        let p = preview(
            "Edit",
            &serde_json::json!({
                "file_path": "src/auth.rs", "old_string": old, "new_string": new,
            }),
        );
        assert!(
            p.contains("- ") && p.contains("let user = legacy(req);"),
            "{p}"
        );
        assert!(
            p.contains("+ ") && p.contains("let user = current(req)?;"),
            "{p}"
        );
        // The signature is identical on both sides and is printed once.
        assert_eq!(
            p.matches("fn handler").count(),
            1,
            "an unchanged line was shown on both sides: {p}"
        );
        // …and the size of the change is stated, so a cut prompt still says how
        // big the thing being approved is.
        assert!(p.contains("+1 -1 lines"), "{p}");
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

    /// **The prompt this feature exists for.** A `Write` over an existing file
    /// used to say the path and the byte count and nothing else — the reader was
    /// asked to approve a change they could not see, which is the exact defect
    /// rule 4 of this module is written against. It now carries the diff, and the
    /// old contents are visibly *going*.
    #[test]
    fn a_write_over_an_existing_file_shows_what_it_destroys() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("auth.rs");
        std::fs::write(&file, "keep me\nDELETE ME\nkeep me too\n").unwrap();
        let p = preview(
            "Write",
            &serde_json::json!({
                "file_path": file.to_str().unwrap(),
                "content": "keep me\nBRAND NEW\nkeep me too\n",
            }),
        );
        assert!(p.contains("- DELETE ME"), "the removal was invisible: {p}");
        assert!(p.contains("+ BRAND NEW"), "{p}");
        assert!(p.contains("+1 -1 lines"), "{p}");
        // Nothing anywhere calls this a new file, which is the mislabelling
        // that makes a destructive prompt read as a harmless one.
        assert!(!p.contains("a new file"), "{p}");
    }

    #[test]
    fn a_huge_edit_is_cut_rather_than_flooding_the_terminal() {
        let big = "line\n".repeat(500);
        let p = preview(
            "Edit",
            &serde_json::json!({ "file_path": "a", "old_string": big, "new_string": "x" }),
        );
        assert!(
            p.lines().count() < crate::term::diff::BUDGET + 8,
            "{}",
            p.lines().count()
        );
        // …and says how much it left out rather than trailing off, which is the
        // rule this repository applied to the web tools hours ago and which
        // matters more here: the reader is approving the part they cannot see.
        assert!(p.contains("more diff lines not shown"), "{p}");
        assert!(
            p.contains("-500 lines") || p.contains("+1 -500 lines"),
            "{p}"
        );
    }
}

// endregion: Tests
