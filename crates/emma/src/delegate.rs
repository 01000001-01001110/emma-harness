//! `Delegate` — hand one self-contained piece of work to a second agent.
//!
//! It lives here rather than in `tools/` for the reason `skill.rs` gives one
//! level down: `tools/fs`, `tools/tasks` and `tools/web` depend on `tool-api`
//! and not on this crate, and a tool that runs the loop needs [`Agent`],
//! [`Setup`], [`Approvals`], [`SessionLog`] and [`Term`] — all of them here.
//! Putting it under `tools/` inverts the dependency and produces a cycle Cargo
//! will refuse. `Skill` is the tool whose content comes from the harness; this
//! is the tool whose content comes from the loop.
//!
//! **It is an ordinary `Tool` and the loop does not know it exists.** `agent.rs`
//! states the invariant: *nothing there branches on a tool's identity, on
//! `ToolError::kind`, or on exit status*. A loop that grew an
//! `if call.name == "Delegate"` arm would be re-acquiring exactly the knowledge
//! two other parts of this codebase have already paid to remove. Everything
//! nesting needs — the shared token meter, the namespaced log, the subordinate
//! terminal — is a value passed *into* `Setup`, not a branch inside it.
//!
//! **Why it is not called `Task`.** `TaskCreate`, `TaskGet`, `TaskList` and
//! `TaskUpdate` already exist. A fifth `Task` collides three times: in the
//! model's own tool list, where the nearest-neighbour error costs a whole
//! subagent rather than a checkbox; in `hooks.rs`, where an operator writing
//! `Task.*` to guard delegation would quietly also guard every checkbox tick;
//! and beside `approval::EXEMPT`, which is a `const &[&str]` one careless edit
//! away from exempting delegation from the gate. Claude Code renamed its own
//! tool away from `Task` too, so matching the old name buys nothing.
//!
//! **Three properties, each with the failure it prevents.**
//!
//! *One at a time.* A one-permit [`Semaphore`] held for the whole nested run.
//! Two subagents would mean two approval prompts racing for one stdin:
//! `decide` calls `prompt_header` and only then `ask`, so two concurrent
//! approvals interleave as A's header, B's header, A's question, B's question —
//! a screen showing B's evidence above A's question, with one `y` going to
//! whichever holds the mutex. That is the defect `LineSource::drain` exists to
//! prevent, arriving by another route, and `drain` makes it *worse*: the second
//! prompt discards the line typed in answer to the first. The closest prior art
//! (goose) resolved the same collision by refusing to run subagents at all
//! unless approval is switched off, which is the escape hatch Emma's design
//! rules out. The permit is redundant today — `run_goal` awaits its tool calls
//! one at a time — and it is what makes a future parallelisation of that loop
//! fail loudly rather than quietly.
//!
//! *No recursion, by construction.* Each agent type's registry is built here,
//! from tools resolved **before this tool exists**, so there is nothing to
//! forget at a call site and no depth counter to thread. An inner model asking
//! for `Delegate` gets the ordinary `no_such_tool` observation and routes around
//! it.
//!
//! *The footer is written from the log.* See [`footer`]. It is the answer to the
//! one failure delegation manufactures rather than merely risks.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use emma_harness::{AgentDef, Harness};
use emma_llm::{Caching, Mode, Provider};
use emma_tool_api::{Registry, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use crate::agent::{Agent, Budgets, Ending, Interrupt, Outcome, Running, Setup, Spend};
use crate::approval::Approvals;
use crate::goal::{Done, DoneCheck, Goal, MarkerClaim};
use crate::session::SessionLog;
use crate::term::Term;

pub const NAME: &str = "Delegate";

// region: What a delegation is bounded by
// ---------------------------------------------------------------------------
// What a delegation is bounded by
//
// Four constants and the arithmetic over them. They are constants rather than
// judgements at the call site for the reason every truncation cap in this
// repository is: a bound nobody can find is a bound nobody can change.
// ---------------------------------------------------------------------------

/// Model calls a subagent gets when its file does not say. Against the
/// parent's default of 60, because iterations are the bound that catches "one
/// cheap tool forever", and an exploration that has not converged in twenty
/// calls is not going to.
const SUB_ITERATIONS: u32 = 20;

/// The most of the *remaining* goal budget one delegation may spend: half.
/// Half rather than all, so a subagent that runs away leaves the parent enough
/// to report what happened and finish — the same argument shape as `compact`
/// targeting half the context cap.
const SUB_TOKEN_SHARE: i64 = 2;

/// Below this much remaining allowance a delegation refuses to start.
///
/// A delegation launched with nothing left returns a perfectly good answer into
/// a goal that then immediately ends, and the work is lost from the conversation
/// even though it was paid for. The refusal is a `tool_result` the model can
/// read and route around — never a missing tool, which would be a capability
/// that silently disappears late in every goal.
const MIN_REMAINING_TOKENS: i64 = 20_000;

/// A subagent's own deadline, capped below the parent's.
///
/// The parent's wall clock keeps running independently and is checked at the
/// parent's next iteration boundary, so a long delegation can overshoot the
/// parent's deadline by up to this. Bounded, and stated rather than discovered.
const SUB_WALL_CLOCK: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// How many paths or commands a footer lists before it says "and N more".
const FOOTER_ITEMS: usize = 12;

// endregion: What a delegation is bounded by

// region: An agent type
// ---------------------------------------------------------------------------
// An agent type
//
// One `agents/<name>.md` resolved against this binary: a prompt, a registry, a
// provider and a budget. The file format is the harness's business; what a name
// in it *means* to a running process is this file's.
// ---------------------------------------------------------------------------

/// How an agent type's model name becomes a client, or `None` for "the
/// parent's, at delegation time". A named type because the signature is long
/// enough that clippy is right about it.
pub type ProviderFor<'a> = dyn Fn(Option<&str>) -> Option<Arc<dyn Provider>> + 'a;

/// One delegation target, resolved.
struct AgentType {
    name: String,
    description: String,
    /// The project's standing instructions and then the agent file's body. Both
    /// halves on purpose: standing rules about evidence and honesty are not
    /// persona-specific, and a delegated agent that does not inherit *report
    /// what the checks actually said* is one that reports what they were
    /// expected to say.
    instructions: String,
    /// This type's own tools. Never contains [`NAME`] — see the module doc.
    tools: Registry,
    /// The client for the model this type's file named, or `None` for "the
    /// parent's, whatever it is when the delegation runs".
    ///
    /// **`None` is not an optimisation, it is the specification.** An agent file
    /// that names nothing is asking to run on whatever the caller is running on,
    /// and since `/model` that is a fact which changes mid-session. Holding a
    /// clone of the parent's client here would freeze it at boot: after a
    /// `/model`, every subagent that named no model would go on using the model
    /// the user had just changed away from, silently, while the status row said
    /// otherwise. Resolved at call time from [`Nest::running`] instead.
    ///
    /// A file that *does* name a model is the opposite case and is built once,
    /// here: the file is a fixed fact and `/model` is not about it.
    provider: Option<Arc<dyn Provider>>,
    max_iterations: u32,
    /// A ceiling from the file, before the share of the remaining allowance is
    /// applied. `None` means "whatever the share allows".
    max_tokens: Option<i64>,
}

/// Everything a nested run borrows from the process it is nested in.
///
/// Shared rather than copied, every one of them for a stated reason: the
/// approval gate and both its grant sets, so a host approved once stays
/// approved and a sub-approval reaches the same keyboard; the session file, so
/// one `grep` still answers what this session did; the interrupt, so one
/// Ctrl-C stops both loops in the right order; the harness, so hooks and the
/// working directory are the project's; and the token meter, so the owner's
/// "one goal, one meter" is the same integer rather than a convention.
pub struct Nest {
    pub harness: Arc<Harness>,
    pub approvals: Arc<Approvals>,
    pub log: Arc<SessionLog>,
    pub term: Arc<Term>,
    pub interrupt: Arc<Interrupt>,
    pub spend: Arc<Spend>,
    pub cwd: PathBuf,
    pub session_id: String,
    pub caching: Caching,
    /// The *parent's* budgets, which is what a sub-budget is derived from.
    pub budgets: Budgets,
    /// The provider in force right now, for an agent type that named no model
    /// of its own. See [`AgentType::provider`] and [`Running`].
    pub running: Running,
}

// endregion: An agent type

// region: The tool
// ---------------------------------------------------------------------------
// The tool
//
// Construction resolves the catalogue once; `invoke` runs one nested goal.
// ---------------------------------------------------------------------------

pub struct Delegate {
    nest: Nest,
    types: BTreeMap<String, AgentType>,
    description: String,
    /// One at a time, process-wide. See the module doc.
    permit: Semaphore,
    seq: AtomicU64,
}

impl Delegate {
    /// Resolve the agent files against this binary's tools, or answer `None`.
    ///
    /// `None` when nothing survived resolution, and it is the same ruling
    /// `Skill::new` makes: offering a capability that cannot work is a trap with
    /// a description attached. The notes say what was dropped and why, because a
    /// catalogue quietly shorter than the directory is the gap nobody notices
    /// until the model cannot find an agent that is plainly there.
    ///
    /// **Unknown tool names are filtered, not fatal, and this diverges from
    /// `Harness::select_tools` deliberately.** A persona's list is the
    /// operator's own boot-time choice and a typo in it is worth refusing to
    /// start over. An agent library is ninety files somebody accumulated for a
    /// different program: measured against a real one, its files name
    /// `TodoWrite`, `Task`, `repl`, `artifacts` and `brave_web_search`, none of
    /// which this binary registers, and bailing would mean Emma does not start
    /// in that directory at all — a rule that fires on correct configuration is
    /// an outage, not a safety property.
    ///
    /// Matching is case-insensitive first, because 14 of those files spell them
    /// `read`, `write`, `edit`. Emma's tool names differ by more than case, so
    /// nothing becomes ambiguous.
    pub fn new(
        nest: Nest,
        defs: &[AgentDef],
        available: &[Arc<dyn Tool>],
        persona_allowed: Option<&[String]>,
        provider_for: &ProviderFor,
    ) -> (Option<Self>, Vec<String>) {
        let mut notes = Vec::new();
        // The persona's allowlist applies first and applies to everything: an
        // agent type must not be a route to a tool the operator excluded from
        // this run. `select_tools` will apply the same list to the parent's
        // registry a moment from now; this is the same filter, one level down.
        let candidates: Vec<Arc<dyn Tool>> = available
            .iter()
            .filter(|t| match persona_allowed {
                Some(allowed) => allowed.iter().any(|a| a == t.name()),
                None => true,
            })
            .cloned()
            .collect();

        let mut types = BTreeMap::new();
        for def in defs {
            let (tools, dropped) = resolve_tools(def, &candidates);
            if !dropped.is_empty() {
                notes.push(format!(
                    "agent `{}`: this build has no {} — the rest of its tools were kept",
                    def.name,
                    dropped.join(", ")
                ));
            }
            if tools.names().is_empty() {
                notes.push(format!(
                    "agent `{}` named no tool this build registers, so it could not do \
                     anything — not offered",
                    def.name
                ));
                continue;
            }
            debug_assert!(
                !tools.names().contains(&NAME),
                "the child registry contains {NAME}: recursion is prevented by this registry \
                 not containing it, so a future edit that builds it from the parent's has \
                 removed the only guard"
            );
            let instructions = match def.instructions.trim().is_empty() {
                true => nest.harness.instructions.clone(),
                false => format!("{}\n\n{}", nest.harness.instructions, def.instructions)
                    .trim_start()
                    .to_string(),
            };
            types.insert(
                def.name.clone(),
                AgentType {
                    name: def.name.clone(),
                    description: def.description.clone(),
                    instructions,
                    tools,
                    provider: provider_for(def.model.as_deref()),
                    max_iterations: def.max_turns.unwrap_or(SUB_ITERATIONS),
                    max_tokens: def.max_tokens,
                },
            );
        }
        if types.is_empty() {
            return (None, notes);
        }
        let description = describe(&types);
        (
            Some(Self {
                nest,
                types,
                description,
                permit: Semaphore::new(1),
                seq: AtomicU64::new(0),
            }),
            notes,
        )
    }

    /// The names a caller may pass, sorted — the closed enum.
    pub fn agent_names(&self) -> Vec<&str> {
        self.types.keys().map(String::as_str).collect()
    }

    /// The tools one agent type may use. For the test that recursion is
    /// prevented by construction rather than by a check.
    pub fn tools_of(&self, agent: &str) -> Vec<&'static str> {
        self.types
            .get(agent)
            .map(|t| t.tools.names())
            .unwrap_or_default()
    }

    /// The budget one delegation gets, or the reason it cannot start.
    ///
    /// Both halves of the arithmetic are here rather than inline in `invoke`
    /// because they are the same fact seen twice: what is left of the parent's
    /// allowance decides whether to run at all *and* how much of it to lend.
    fn sub_budgets(&self, ty: &AgentType) -> Result<Budgets, ToolError> {
        let remaining = self.nest.budgets.max_tokens - self.nest.spend.get();
        if remaining < MIN_REMAINING_TOKENS {
            // `Unavailable` and phrased as "I cannot", per `ToolError`'s own
            // rule: this is the machinery being out of room, not the work being
            // impossible.
            return Err(ToolError::Unavailable(format!(
                "I cannot start a delegation: this goal has {remaining} of its {} token \
                 allowance left and a subagent needs at least {MIN_REMAINING_TOKENS}. Do the \
                 work here, or tell the user the budget needs raising.",
                self.nest.budgets.max_tokens
            )));
        }
        let share = remaining / SUB_TOKEN_SHARE;
        Ok(Budgets {
            max_iterations: ty.max_iterations,
            max_tokens: ty.max_tokens.map_or(share, |cap| cap.min(share)),
            wall_clock: SUB_WALL_CLOCK.min(self.nest.budgets.wall_clock),
            // One nudge. A subagent that has stopped without claiming completion
            // has usually said most of what it found, and the parent gets that
            // text either way — a second and third nudge spends the parent's
            // budget arguing with a run the parent cannot see.
            max_kicks: 1,
            max_context: self.nest.budgets.max_context,
        })
    }
}

/// The tools one agent file resolves to, and the names that matched nothing.
fn resolve_tools(def: &AgentDef, candidates: &[Arc<dyn Tool>]) -> (Registry, Vec<String>) {
    let mut registry = Registry::new();
    let Some(wanted) = def.tools.as_deref() else {
        // Absent — and, deliberately, empty — means inherit. See
        // `claude::Tools::allowlist`, which is where that ruling is argued.
        for tool in candidates {
            registry.register(tool.clone());
        }
        return (registry, Vec::new());
    };
    let mut dropped = Vec::new();
    for want in wanted {
        if !candidates
            .iter()
            .any(|t| t.name().eq_ignore_ascii_case(want))
        {
            dropped.push(want.clone());
        }
    }
    // Registry order rather than the file's order, for the reason
    // `select_tools` gives: the tool schema rides in the cached prompt prefix,
    // so the bytes must not depend on how somebody typed a list.
    for tool in candidates {
        if wanted.iter().any(|w| w.eq_ignore_ascii_case(tool.name())) {
            registry.register(tool.clone());
        }
    }
    (registry, dropped)
}

/// The catalogue the description carries, composed at load and sorted.
///
/// Same mechanism as `Skill`'s, and the same answer to the objection that a
/// description cannot vary by machine because it is hashed:
/// `Registry::schema_hash`'s own doc says it is a change detector for a human
/// reading a log, not an identity. The real constraint is byte-stability
/// *between calls on one machine*, which sorting satisfies.
fn describe(types: &BTreeMap<String, AgentType>) -> String {
    let mut out = String::from(DESCRIPTION);
    out.push_str("\n\nAvailable:\n");
    for ty in types.values() {
        out.push_str(&format!(
            "\n- `{}` — {}",
            ty.name,
            one_line(&ty.description)
        ));
    }
    out.push('\n');
    out
}

/// An agent's description, on one line, cut if it is long — and **saying so in
/// the terms `INV-011` requires**.
///
/// **A bare `…` here is the worst place in the codebase to cut silently.** This
/// string is what `describe()` puts in the `Task` tool's schema, which is the
/// only text the calling model has to choose a delegation target with. A model
/// reading a sentence that stops mid-clause has no way to tell a short
/// description from an amputated one, and the thing it is deciding is which
/// agent to hand the work to.
///
/// Measured against the real library on this machine when the row was filed:
/// **11 of 86 agent descriptions were over the cap** — one of them 450
/// characters — every one of them cut with a bare ellipsis and no cap named.
///
/// `INV-011` is stated as *"every place output is cut names the cap, the loss
/// and the remedy, or says plainly that no argument raises it"*. Its
/// preservation evidence covered `ToolOutcome` truncation only, so this was
/// outside the guard rather than exempt from the rule.
///
/// There is no remedy to offer — the cap is a constant and no argument reaches
/// it — so it says that, which is the branch the invariant provides for.
fn one_line(text: &str) -> String {
    const CAP: usize = 300;
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let total = text.chars().count();
    if total > CAP {
        format!(
            "{}… [cut at {CAP} of {total} characters; no argument raises that — read the \
             agent's own file for the rest]",
            text.chars().take(CAP).collect::<String>()
        )
    } else {
        text
    }
}

/// The prose half of the description.
///
/// Roughly half of it is about when *not* to call this, and that ratio is
/// deliberate: the measured direction of the effect on code-centric tasks is
/// negative — 49–54% single-agent against 10% and 3% multi-agent on SWE-bench
/// Lite, at nine times the spend — and Anthropic's own multi-agent write-up
/// carves coding out of its own headline result. A feature nobody calls costs a
/// paragraph of prompt; a feature called for two-file questions costs a multiple
/// of every goal.
///
/// "Read the ending first" is a promise about the footer, and the footer is
/// built in the same commit. A description advertising something that does not
/// exist is the highest-traffic lie available in this system.
const DESCRIPTION: &str = "\
Hand a self-contained piece of work to a second agent, and get back its \
conclusion.

It starts with none of this conversation: not what you have read, not what the \
user said, not what you have already tried. It runs on its own until it is \
finished, and the only thing that comes back is text — its report, plus a \
record of what it actually did. Nothing it read enters this conversation.

**Reach for this when the answer is small and the evidence is large.** \"Which \
of these forty files still calls the old constructor\", \"why does this suite \
fail on Windows and not on Linux\" — questions where finding out means dozens \
of reads whose contents you will never need again.

**Do not reach for it otherwise.** A question you can answer with one `Grep` \
costs one `Grep`; delegating it costs a second agent's whole startup and a \
brief you have to write. If you already have the file open, answer from it. If \
the work is a single edit, make the edit. And do not delegate the check that \
verifies your own work — run that yourself, and read its exit status.

- `agent` is which kind of agent to send this to. Each has its own standing \
instructions and its own tools; the list is below.
- `task` is the work, written for someone who has never seen this conversation, \
because that is exactly who reads it. State the goal and what a finished answer \
looks like.
- `context` is what you already know that it would otherwise rediscover — one \
short fact per entry. Paths worth starting from, the command that failed and \
what it printed, a decision already made, and anything you already tried that \
did not work. Prefer a path over a paste: it can read the file itself, and what \
it reads will be current. Do not paste this conversation.
- `deliver` is what the answer must contain. Be concrete — \"the file and line \
where X is decided\", \"the failing assertion, verbatim\" — because you cannot \
see its work and will have nothing else to judge the answer by. Ask for places \
to look wherever that will do: a list of paths is something you can check by \
reading one of them, and an explanation is something you cannot check at all.

What comes back is one message, and under it a record this agent did not write: \
how it ended, the files it read, the commands it ran and what they exited with, \
anything it was refused, and what it spent. **Read the ending first.** An agent \
that ran out of budget still writes a confident final paragraph, and that \
paragraph is a partial answer wearing the clothes of a complete one.

It works in the same directory as you, under the same approval rules, and \
spends from the same budget as this goal — so a long delegation can end the \
goal. One runs at a time.";

#[async_trait::async_trait]
impl Tool for Delegate {
    fn name(&self) -> &'static str {
        NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    // The closed enum, and it is the security property — the same
                    // one `skill.rs` names. A `Delegate` taking a free-text system
                    // prompt, or a free-text tool list, is a self-modifying-prompt
                    // tool with a friendly description, and worse than the
                    // arbitrary-file-read it resembles: the model would be
                    // granting itself a prompt *and* a tool set.
                    "enum": self.agent_names(),
                    "description": "Which kind of agent to hand this to."
                },
                "task": {
                    "type": "string",
                    "description": "The work, written for someone who has never seen this \
                                    conversation."
                },
                "context": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "One short fact per entry, including what you already \
                                    tried that did not work. Prefer a path over a paste."
                },
                "deliver": {
                    "type": "string",
                    "description": "What the answer must contain, concretely."
                }
            },
            "required": ["agent", "task"],
            "additionalProperties": false
        })
    }

    /// **`read_only: false` unconditionally, including when the agent type's
    /// tools are entirely read-only.** The tool set is configuration; a `meta()`
    /// that varied with it would mean the gate's answer depends on a file the
    /// reviewer is not looking at. This is also the one call in the surface
    /// where a human most wants to see the brief before it runs: it is about to
    /// spend a slice of the budget on instructions the *model* composed.
    ///
    /// `idempotent: false` follows, and the pair is legal — `Registry::register`
    /// refuses only `read_only: true` with `idempotent: false`.
    ///
    /// **`reaches_network: false`, and it is the arguable one.** A delegation
    /// certainly sends bytes off this machine: it calls the model. Three reasons
    /// it still declares `false`. Its own traffic goes to the provider, on the
    /// same key and host the parent is already using ungated, and declaring
    /// `true` would demand a `NetworkTarget` and produce a prompt to approve
    /// `api.anthropic.com` whose only possible answer is yes — which is how an
    /// operator learns to answer without reading. Any sub-tool that reaches a
    /// real host asks the same gate, in the same process, against the same
    /// `hosts_allowed` set, because `Approvals` is one shared object: egress is
    /// gated at the leaf, where the host is known, not at the delegation, where
    /// it is not. And it is `Bash`'s precedent — `Bash` can `curl` and declares
    /// `false`, because `read_only: false` already puts the thing in front of a
    /// human, and a truthful destination would require predicting what a model
    /// will decide to do.
    ///
    /// If that trade ever looks wrong the fix is not to flip the bit; it is a
    /// third axis on `ToolMeta` for *delegated* egress.
    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: false,
            reaches_network: false,
            idempotent: false,
        }
    }

    fn validate_args(&self, args: &Value) -> Result<(), ToolError> {
        let agent = args.get("agent").and_then(Value::as_str).ok_or_else(|| {
            ToolError::BadArguments(format!(
                "{NAME}.agent is required and must be one of: {}",
                self.agent_names().join(", ")
            ))
        })?;
        // The enum is enforced here as well as declared to the model, for the
        // reason `Skill` gives: a provider that ignores an enum must not turn
        // this into something that runs a prompt nobody wrote.
        if !self.types.contains_key(agent) {
            return Err(ToolError::BadArguments(format!(
                "no agent named `{agent}`. Available: {}",
                self.agent_names().join(", ")
            )));
        }
        match args.get("task").and_then(Value::as_str) {
            Some(task) if !task.trim().is_empty() => Ok(()),
            _ => Err(ToolError::BadArguments(format!(
                "{NAME}.task is required: say what the work is, written for someone who has \
                 never seen this conversation."
            ))),
        }
    }

    async fn invoke(
        &self,
        ctx: &ToolCtx,
        args: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        let agent = args
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(ty) = self.types.get(agent) else {
            return Ok(Err(ToolError::BadArguments(format!(
                "no agent named `{agent}`. Available: {}",
                self.agent_names().join(", ")
            ))));
        };
        let budgets = match self.sub_budgets(ty) {
            Ok(budgets) => budgets,
            Err(e) => return Ok(Err(e)),
        };
        let brief = brief(&args);

        // Held for the whole nested run. See the module doc: this is what makes
        // a second delegation wait rather than race the first for the keyboard.
        let _permit = self.permit.acquire().await;

        let n = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let sub_id = format!("sub-{}-{n}", ctx.turn_id);
        let (log, records) = self.nest.log.subagent(&sub_id, &ctx.turn_id, agent);
        let term = self.nest.term.subordinate();
        self.nest.term.note(&format!(
            "delegating to {agent} ({} tools, {} tokens): {}",
            ty.tools.names().len(),
            budgets.max_tokens,
            one_line(args.get("task").and_then(Value::as_str).unwrap_or_default())
        ));

        let done = SubagentClaim {
            deliver: args
                .get("deliver")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        let outcome: Outcome = {
            let mut sub = Agent::new(Setup {
                // **A subagent gets its own registry, not the parent's.** Its
                // session id is its own, so sharing one would let a child read
                // and kill work the parent started, and the ids are short and
                // sequential enough to hit by accident. A subagent's background
                // work also ends with the subagent, which a separate registry
                // makes structural rather than a rule somebody has to remember.
                background: Default::default(),
                provider: ty
                    .provider
                    .clone()
                    .unwrap_or_else(|| self.nest.running.get()),
                harness: &self.nest.harness,
                instructions: &ty.instructions,
                tools: &ty.tools,
                approvals: &self.nest.approvals,
                log: &log,
                term: &term,
                interrupt: self.nest.interrupt.clone(),
                // Its own meter, charging the parent's as it goes. One goal, one
                // meter, in the strongest sense available: the same integer.
                spend: Spend::child(&self.nest.spend),
                done: &done,
                cwd: self.nest.cwd.clone(),
                session_id: self.nest.session_id.clone(),
                budgets,
                caching: self.nest.caching,
                // Never `Stream`: a delegation reports once at the end, and a
                // subordinate `Term` swallows prose anyway, so streaming it
                // would be bytes nobody reads.
                mode: Mode::Batch,
            });
            sub.run_goal(&Goal::new(brief)).await
        };

        let seen = records.lock().map(|r| r.clone()).unwrap_or_default();
        let facts = Facts::from(&seen);
        let footer = facts.footer();
        self.nest.term.note(&format!("{agent} {}", facts.summary()));
        // The parent-level record: one line per delegation carrying everything
        // "is this agent type worth using?" needs, and the `cost_tokens` a
        // resumed parent adds back to its meter. `emma agents` reads these.
        self.nest.log.append(
            "delegation",
            json!({
                "sub_id": sub_id,
                "parent_turn_id": ctx.turn_id,
                "agent": agent,
                "task": args.get("task"),
                "ending": outcome.ending.as_str(),
                "cost_tokens": outcome.tokens,
                "iterations": outcome.iterations,
                "elapsed_ms": facts.elapsed_ms,
                "tool_calls": facts.tool_calls,
                // `files_read` keeps its name though the footer's word is now
                // "touched": `emma agents` reads these keys, and renaming a
                // JSONL field to match a cosmetic rewording breaks a consumer
                // for nothing.
                "files_read": facts.files.len(),
                "fetched": facts.fetched.len(),
                "other_tool_calls": facts.other.values().sum::<usize>(),
                "commands": facts.commands.len(),
                "failed_commands": facts.commands.iter().filter(|c| c.1 != Some(0)).count(),
                "denied": facts.denied.len(),
                "max_tokens": budgets.max_tokens,
                "max_iterations": budgets.max_iterations,
            }),
        );

        let body = format!("{}\n\n{footer}", outcome.text.trim());
        Ok(match outcome.ending {
            // `main.rs` already treats these two together as success: a brief
            // needing no tools was still answered.
            Ending::Done | Ending::Answered => {
                Ok(ToolOutcome::new(body).with_display(facts.summary()))
            }
            // The machinery, not the work.
            Ending::Provider(ref e) => Err(ToolError::Unavailable(format!(
                "the delegated agent's provider failed: {e}"
            ))),
            // Everything else did work and did not claim completion. The text is
            // usually most of the answer, and throwing it away to return a clean
            // error throws away everything that was paid for — so it is carried,
            // under an ending that says not to re-issue the same brief.
            _ => Err(ToolError::Failed(format!(
                "the delegated agent stopped without finishing: {}\n\nWhat it had at that \
                 point, and what it actually did:\n\n{body}\n\nRe-issuing the same brief will \
                 hit the same wall. Narrow it, or do the work here.",
                outcome.ending.message(&budgets)
            ))),
        })
    }
}

/// The brief the subagent opens on: the three fields, in the order it should
/// read them.
///
/// Composed here rather than in `Goal::opening`, which is *the user's words and
/// nothing else* and must stay that way — the note above it records what a
/// preamble on every input cost. This text is not a user's words: it is a work
/// order the calling model wrote, and the fields are its own.
fn brief(args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or_default();
    let mut out = s("task").trim().to_string();
    if let Some(context) = args.get("context").and_then(Value::as_array) {
        let facts: Vec<String> = context
            .iter()
            .filter_map(Value::as_str)
            .filter(|f| !f.trim().is_empty())
            .map(|f| format!("- {}", f.trim()))
            .collect();
        if !facts.is_empty() {
            out.push_str("\n\nWhat the agent that sent you already knows:\n");
            out.push_str(&facts.join("\n"));
        }
    }
    let deliver = s("deliver").trim();
    if !deliver.is_empty() {
        out.push_str(&format!("\n\nYour answer must contain: {deliver}"));
    }
    out
}

// endregion: The tool

// region: The done-check a delegation runs under
// ---------------------------------------------------------------------------
// The done-check a delegation runs under
//
// The second `impl DoneCheck` this codebase has, and the seam `goal.rs` left
// for it: the loop knows only the trait, so this is a different value in
// `Setup` rather than surgery anywhere.
// ---------------------------------------------------------------------------

/// [`MarkerClaim`], plus the one thing a subagent has to be told that a parent
/// does not.
///
/// The framing goes in the **contract** — which `standing_contract` puts in the
/// system prompt, where it is cached — and never on the brief. `Goal::opening`
/// is the user's words and nothing else; here the "user" is a work order, and
/// prepending to it would still be an instruction the caller did not write.
struct SubagentClaim {
    /// Restated in the contract when the caller asked for something specific.
    /// This is the field that makes an answer checkable against the footer: a
    /// `deliver` naming a file and line, or a command's verbatim output, is one
    /// the parent can hold up against the record underneath it.
    deliver: Option<String>,
}

#[async_trait::async_trait]
impl DoneCheck for SubagentClaim {
    /// Distinct from `marker_claim`, so a transcript carrying both runs says
    /// which authority each was held to.
    fn name(&self) -> &'static str {
        "subagent_marker_claim"
    }

    fn contract(&self) -> String {
        let mut out = MarkerClaim.contract();
        out.push_str(
            "\n\nYou are working on behalf of another agent, which cannot see any of this. \
             Your final message is the only thing that reaches it: no transcript, no tool \
             output, no file you read. Write it so it stands alone, and name the files you \
             read and the commands you ran — it will be shown a record of both, and a claim \
             that disagrees with the record is worse than no claim.",
        );
        if let Some(deliver) = &self.deliver {
            out.push_str(&format!(
                "\n\nThe agent that sent you asked specifically for: {deliver}"
            ));
        }
        out
    }

    async fn verdict(&self, goal: &Goal, text: &str) -> Done {
        // Deliberately the marker and nothing more. A check that also judged the
        // answer against `deliver` would be a heuristic wearing a mechanism's
        // clothes — and it is `open_count`'s failure in a new costume: a model
        // that learns running a command ends the loop will run one. The footer
        // is where a `deliver` is checked, by the parent, against a record.
        MarkerClaim.verdict(goal, text).await
    }
}

// endregion: The done-check a delegation runs under

// region: The footer, which the subagent did not write
// ---------------------------------------------------------------------------
// The footer, which the subagent did not write
//
// The part of this design that matters. A subagent's deliverable is by
// construction a description of work rather than the work, and the failure that
// produces is documented rather than hypothetical: *"subagents imply they read
// file content but demonstrably did not — they infer behavior from filenames,
// variable names, or comments instead of tracing actual code logic"*
// (anthropics/claude-code#40339, closed as not planned).
//
// Requiring the subagent to cite what it read is an improvement and is still an
// assertion that looks like a verification: a run that hallucinated a conclusion
// will hallucinate a plausible list of files to go with it. The harness already
// knows. Every one of these numbers comes out of the records the *loop* wrote
// during that run — `Facts::from` takes them and nothing else, and takes no
// text the model produced.
//
// **Every recorded tool call appears in exactly one footer category, and a
// call can be described coarsely but can never vanish.** That is the invariant
// the categories are arranged around, and it is a repair rather than an
// original virtue: the categories are keyed on argument names, so for a while
// any call carrying none of `file_path`/`pattern`/`command` — `WebFetch(url)`,
// `WebSearch(query)`, `Skill`, every `Task*` — was counted in `tool_calls` and
// named nowhere, which reads as `files read: none` under a paragraph
// describing five fetches. Coarseness is visible and self-correcting; a parent
// reading `other tool calls: Frobnicate ×2` knows the record is coarse.
// Silence is not: it is the footer partly believing the account it exists to
// check.
//
// **What it proves and what it does not.** It proves what was looked at, what
// was run, and how the run ended. It does not prove the conclusion follows from
// any of it. The worst case of this whole design is that the footer becomes the
// thing everyone trusts — `done · 14 files · 3 commands` reads like verification
// and is only attendance. It reports counts rather than adjectives for that
// reason, and nothing anywhere claims the conclusion was checked.
// ---------------------------------------------------------------------------

/// What one delegation actually did, read out of its own log.
#[derive(Default)]
struct Facts {
    ending: String,
    detail: Option<String>,
    tokens: i64,
    iterations: u64,
    elapsed_ms: u64,
    tool_calls: usize,
    files: Vec<String>,
    searches: Vec<String>,
    /// Where it went. `url` is `WebFetch`'s argument name, and egress is the
    /// class of call the rest of this design treats as the sensitive one — a
    /// footer that reported five fetches as `files read: none` was
    /// under-reporting precisely there.
    fetched: Vec<String>,
    /// Every call whose arguments matched no probe above, tallied by the tool
    /// name **the loop recorded**. Coarse on purpose, and the reason it exists
    /// is in the region doc: a call may be described coarsely, never omitted.
    other: BTreeMap<String, usize>,
    /// The command, and the exit status its result reported. `None` is a command
    /// whose result never arrived — interrupted, or refused.
    commands: Vec<(String, Option<i64>)>,
    denied: Vec<String>,
}

impl Facts {
    /// **The mutation to try on this function:** compose it from the subagent's
    /// final message instead of from `records`. `tests/delegate.rs` has a test
    /// whose scripted subagent claims to have read files it never opened and run
    /// a command that never ran; it goes red the moment this reads testimony
    /// rather than the log.
    fn from(records: &[Value]) -> Self {
        let mut facts = Self::default();
        let mut by_id: std::collections::HashMap<String, (String, Value)> = Default::default();
        for record in records {
            match record["kind"].as_str().unwrap_or_default() {
                "sub.tool_call" => {
                    facts.tool_calls += 1;
                    let (Some(id), Some(tool)) = (str_of(record, "id"), str_of(record, "tool"))
                    else {
                        continue;
                    };
                    let args = record["args"].clone();
                    // Keyed on the argument rather than on the tool's name, the
                    // same way `agent::compact` and `term::summarise_args`
                    // already are: this file has no business knowing which
                    // tools a build registers, and an agent library names tools
                    // that do not exist here at all.
                    //
                    // **The last arm is the one that makes the convention
                    // safe.** Keying on argument names means a tool naming its
                    // arguments something else falls through every probe, and
                    // until this arm existed it fell through into silence —
                    // Emma's own `WebFetch(url)` and `WebSearch(query)` among
                    // them, which is how five fetches footered as
                    // `files read: none` over `5 tool calls`. The residual is
                    // keyed on the tool name out of the record, which is a
                    // fact the loop wrote and not knowledge of the registry.
                    if let Some(path) = str_of(&args, "file_path") {
                        push_unique(&mut facts.files, path);
                    } else if let Some(pattern) = str_of(&args, "pattern") {
                        push_unique(&mut facts.searches, pattern);
                    } else if let Some(command) = str_of(&args, "command") {
                        facts.commands.push((one_line(&command), None));
                    } else if let Some(url) = str_of(&args, "url") {
                        push_unique(&mut facts.fetched, url);
                    } else if let Some(query) = str_of(&args, "query") {
                        // A search by any other argument name is still a
                        // search, so it joins the list it belongs in rather
                        // than earning a line of its own.
                        push_unique(&mut facts.searches, query);
                    } else {
                        *facts.other.entry(tool.clone()).or_default() += 1;
                    }
                    by_id.insert(id, (tool, args));
                }
                "sub.tool_result" => {
                    let Some(id) = str_of(record, "id") else {
                        continue;
                    };
                    let Some((_, args)) = by_id.get(&id) else {
                        continue;
                    };
                    let Some(command) = str_of(args, "command") else {
                        continue;
                    };
                    let content = record["block"]["content"].as_str().unwrap_or_default();
                    // The field first. Parsing the prose is the fallback for
                    // records written before the field existed, and it is the
                    // reason this feature never worked: `Bash` puts its shell
                    // banner above the status line, so a parser reading the
                    // first line found the banner every time.
                    let status = record["exit_code"]
                        .as_i64()
                        .or_else(|| exit_status(content));
                    let command = one_line(&command);
                    if let Some(slot) = facts
                        .commands
                        .iter_mut()
                        .find(|(c, s)| *c == command && s.is_none())
                    {
                        slot.1 = status;
                    }
                }
                "sub.denied" => {
                    let by = str_of(record, "by").unwrap_or_else(|| "?".into());
                    let what = str_of(record, "id")
                        .and_then(|id| by_id.get(&id).map(|(tool, _)| tool.clone()))
                        .unwrap_or_else(|| "a call".into());
                    push_unique(
                        &mut facts.denied,
                        format!(
                            "{what} ({})",
                            if by == "hook" { "policy" } else { "declined" }
                        ),
                    );
                }
                "sub.goal_finished" => {
                    facts.ending = string(record, "ending");
                    facts.detail = record["detail"].as_str().map(str::to_string);
                    facts.tokens = record["tokens"].as_i64().unwrap_or(0);
                    facts.iterations = record["iterations"].as_u64().unwrap_or(0);
                    facts.elapsed_ms = record["elapsed_ms"].as_u64().unwrap_or(0);
                }
                _ => {}
            }
        }
        facts
    }

    /// One line, for the terminal and for the tool's `display`.
    fn summary(&self) -> String {
        format!(
            "{} · {} tool calls · {} files · {} commands · {}s · {} tokens",
            self.ending,
            self.tool_calls,
            self.files.len(),
            self.commands.len(),
            self.elapsed_ms / 1000,
            self.tokens
        )
    }

    /// What the parent reads under the subagent's own words.
    ///
    /// The ending is first because it is the single most valuable field here:
    /// [`Ending::Tokens`] and [`Ending::Done`] otherwise leave the same
    /// confident final paragraph, and without this line the parent cannot tell
    /// a finished run from a truncated one.
    fn footer(&self) -> String {
        let mut out = String::from(
            "--- what this agent actually did (recorded by the harness, \
                                    not written by the agent) ---\n",
        );
        out.push_str(&format!(
            "ended: {}{}\n",
            if self.ending.is_empty() {
                "unknown — no record of how this run stopped"
            } else {
                &self.ending
            },
            match &self.detail {
                Some(detail) => format!(" ({detail})"),
                None => String::new(),
            }
        ));
        if self.ending != "done" && self.ending != "answered" && !self.ending.is_empty() {
            out.push_str(
                "note: it did not claim the work was finished, so read what follows as partial.\n",
            );
        }
        // "touched", not "read": `Write` and `Edit` carry `file_path` too, so
        // a written file has always landed in this list. The neutral word
        // costs nothing and stops the over-claim; splitting read from written
        // would need the tool-name knowledge this file has deliberately
        // refused.
        out.push_str(&listed("files touched", &self.files));
        out.push_str(&listed("searched for", &self.searches));
        out.push_str(&listed("fetched", &self.fetched));
        let commands: Vec<String> = self
            .commands
            .iter()
            .map(|(command, status)| match status {
                Some(code) => format!("{command} → exit {code}"),
                None => format!("{command} → no result (refused, or the run stopped)"),
            })
            .collect();
        out.push_str(&listed("commands run", &commands));
        // The residual. Named only when there is something in it — an empty
        // bucket has nothing to disclose, and the categories above already say
        // `none` for themselves.
        if !self.other.is_empty() {
            let other: Vec<String> = self
                .other
                .iter()
                .map(|(tool, n)| format!("{tool} ×{n}"))
                .collect();
            out.push_str(&listed("other tool calls", &other));
        }
        if !self.denied.is_empty() {
            out.push_str(&listed("refused or blocked", &self.denied));
        }
        out.push_str(&format!(
            "spent: {} model calls, {} tokens, {}s\n",
            self.iterations,
            self.tokens,
            self.elapsed_ms / 1000
        ));
        out
    }
}

/// A capped list, and an honest count when it is capped. Never present a cut
/// list as a whole one — the same discipline `read.md` and `bash.md` state.
fn listed(what: &str, items: &[String]) -> String {
    if items.is_empty() {
        return format!("{what}: none\n");
    }
    let shown: Vec<&String> = items.iter().take(FOOTER_ITEMS).collect();
    let mut out = format!("{what} ({}):\n", items.len());
    for item in shown {
        out.push_str(&format!("  {item}\n"));
    }
    if items.len() > FOOTER_ITEMS {
        out.push_str(&format!("  … and {} more\n", items.len() - FOOTER_ITEMS));
    }
    out
}

/// `Bash` carries `exit status <n>` as the first line of its content, and the
/// loop treats a non-zero exit as an ordinary result. A footer line reading
/// `cargo test → exit 101` beside a final message reading "the tests pass" is a
/// contradiction the parent can see, and it is the most common lie a coding
/// subagent can tell.
/// The exit status stated in a command's prose, for records written before
/// `exit_code` was a field.
///
/// **Scans the opening lines rather than only the first.** `Bash` prepends its
/// shell banner to every result, so the status has never been on line one, and
/// a parser that read only line one returned `None` for every command ever run
/// by a subagent — which is why the footer reported "no result" throughout.
fn exit_status(content: &str) -> Option<i64> {
    content
        .lines()
        .take(4)
        .find_map(|l| l.trim().strip_prefix("exit status "))?
        .trim()
        .parse()
        .ok()
}

fn push_unique(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

fn str_of(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

fn string(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

// endregion: The footer, which the subagent did not write

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The pure halves only: what a footer says about a record stream, and how an
// agent file's tool list resolves against a build that does not have all of it.
// Everything that needs a nested loop is driven end to end from
// `tests/delegate.rs`, against the real `Agent`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, tool: &str, args: Value) -> Value {
        json!({ "kind": "sub.tool_call", "id": id, "tool": tool, "args": args })
    }

    fn result(id: &str, content: &str) -> Value {
        json!({
            "kind": "sub.tool_result",
            "id": id,
            "block": { "type": "tool_result", "tool_use_id": id, "content": content },
        })
    }

    #[test]
    fn a_commands_exit_status_reaches_the_footer() {
        // The contradiction this exists to make visible: a final message saying
        // the tests pass, over a record saying they exited 101.
        let facts = Facts::from(&[
            call("t1", "Bash", json!({ "command": "cargo test" })),
            result("t1", "exit status 101\n\nfailures:\n  loop::budget"),
            json!({ "kind": "sub.goal_finished", "ending": "done", "tokens": 400,
                    "iterations": 2, "elapsed_ms": 3_000 }),
        ]);
        let footer = facts.footer();
        assert!(footer.contains("cargo test → exit 101"), "{footer}");
        assert!(footer.contains("ended: done"), "{footer}");
    }

    #[test]
    fn an_ending_that_is_not_done_is_called_out_rather_than_merely_named() {
        // `Ending::Tokens` and `Ending::Done` leave the same confident final
        // paragraph. This line is the whole difference available to the parent.
        let facts = Facts::from(&[json!({
            "kind": "sub.goal_finished", "ending": "tokens", "tokens": 90_000,
            "iterations": 9, "elapsed_ms": 60_000,
        })]);
        let footer = facts.footer();
        assert!(footer.contains("ended: tokens"), "{footer}");
        assert!(footer.contains("partial"), "{footer}");
    }

    #[test]
    fn a_run_with_no_record_of_its_ending_says_so_rather_than_implying_success() {
        let footer = Facts::from(&[]).footer();
        assert!(footer.contains("unknown"), "{footer}");
        assert!(footer.contains("files touched: none"), "{footer}");
        assert!(footer.contains("fetched: none"), "{footer}");
        // The residual is the one category that stays quiet when empty: there
        // is nothing to disclose, and `other tool calls: none` on every footer
        // is noise rather than honesty.
        assert!(!footer.contains("other tool calls"), "{footer}");
    }

    #[test]
    fn the_arguments_emmas_own_web_tools_use_reach_the_footer() {
        // Not hypothetical: `WebFetch` takes `url` and `WebSearch` takes
        // `query`, and neither is `file_path`/`pattern`/`command`. Before the
        // probe covered them, a subagent that fetched five pages footered as
        // `files touched: none · searched for: none · commands run: none` over
        // `5 tool calls` — under-reporting the exact class of call the rest of
        // this design treats as the sensitive one.
        let facts = Facts::from(&[
            call("t1", "WebFetch", json!({ "url": "https://apnews.com/x" })),
            call("t2", "WebSearch", json!({ "query": "rust idna" })),
            json!({ "kind": "sub.goal_finished", "ending": "done" }),
        ]);
        let footer = facts.footer();
        assert!(footer.contains("fetched (1)"), "{footer}");
        assert!(footer.contains("https://apnews.com/x"), "{footer}");
        assert!(footer.contains("searched for (1)"), "{footer}");
        assert!(footer.contains("rust idna"), "{footer}");
        // …and neither is left in the coarse bucket as well as its own list.
        assert!(!footer.contains("other tool calls"), "{footer}");
    }

    #[test]
    fn a_call_whose_arguments_match_no_probe_is_named_coarsely_rather_than_dropped() {
        let facts = Facts::from(&[
            call("t1", "Frobnicate", json!({ "target": "x" })),
            call("t2", "Frobnicate", json!({ "target": "y" })),
            call("t3", "TaskCreate", json!({ "title": "ship it" })),
            json!({ "kind": "sub.goal_finished", "ending": "done" }),
        ]);
        let footer = facts.footer();
        assert!(footer.contains("Frobnicate ×2"), "{footer}");
        assert!(footer.contains("TaskCreate ×1"), "{footer}");
    }

    #[test]
    fn a_recognised_call_is_in_its_own_category_and_not_also_in_the_residual() {
        // The residual must be the complement of the other lists, not a second
        // copy of them: a parent that reads `Read ×3` beside three named files
        // learns nothing and doubts both.
        let facts = Facts::from(&[
            call("t1", "Read", json!({ "file_path": "src/a.rs" })),
            call("t2", "Grep", json!({ "pattern": "fn main" })),
            call("t3", "Bash", json!({ "command": "cargo test" })),
            call("t4", "WebFetch", json!({ "url": "https://docs.rs" })),
            json!({ "kind": "sub.goal_finished", "ending": "done" }),
        ]);
        let footer = facts.footer();
        assert!(!footer.contains("other tool calls"), "{footer}");
        for named in ["Read", "Grep", "Bash", "WebFetch"] {
            assert!(!footer.contains(&format!("{named} ×")), "{footer}");
        }
    }

    #[test]
    fn every_recorded_call_lands_in_exactly_one_footer_category() {
        // The invariant, asserted as arithmetic rather than as prose: the
        // categories partition `tool_calls`. A future probe added to the chain
        // that forgets the residual, or a residual that double-counts, breaks
        // this without anybody having to think of the tool it would happen to.
        //
        // Every argument below is distinct, deliberately: the lists dedupe, so
        // the arithmetic is a partition only over calls that differ. The
        // property under test is that no call is missing from every list, and
        // duplicates would hide that behind a smaller number.
        let records = vec![
            call("t1", "Read", json!({ "file_path": "src/a.rs" })),
            call("t2", "Read", json!({ "file_path": "src/b.rs" })),
            call("t3", "Grep", json!({ "pattern": "fn main" })),
            call("t4", "Bash", json!({ "command": "cargo test" })),
            call("t5", "WebFetch", json!({ "url": "https://docs.rs" })),
            call("t6", "WebSearch", json!({ "query": "rust idna" })),
            call("t7", "Skill", json!({ "name": "release" })),
            call("t8", "TaskCreate", json!({ "title": "ship it" })),
            json!({ "kind": "sub.goal_finished", "ending": "done" }),
        ];
        let facts = Facts::from(&records);
        let categorised = facts.files.len()
            + facts.searches.len()
            + facts.fetched.len()
            + facts.commands.len()
            + facts.other.values().sum::<usize>();
        assert_eq!(
            categorised,
            facts.tool_calls,
            "{} of {} calls are only in the count: {}",
            facts.tool_calls - categorised,
            facts.tool_calls,
            facts.footer()
        );
        // And each tool that ran is findable somewhere in the footer text the
        // parent actually reads.
        let footer = facts.footer();
        for name in ["Skill", "TaskCreate"] {
            assert!(footer.contains(name), "{name} vanished: {footer}");
        }
    }

    #[test]
    fn a_file_opened_twice_is_listed_once_and_still_counted_twice() {
        // The two halves disagree on purpose, and both are true: `tool_calls`
        // is attendance and the list is what was looked at. Turning
        // `push_unique` into a plain `push` — one line — leaves a footer
        // reading `files touched (2)` over the same path twice, which reads as
        // twice the work.
        //
        // The command is the control on the other side of the same convention:
        // commands are *not* deduped, because two runs of `cargo test` either
        // side of an edit are two facts and the exit codes are the point.
        let facts = Facts::from(&[
            call("t1", "Read", json!({ "file_path": "src/a.rs" })),
            call("t2", "Read", json!({ "file_path": "src/a.rs" })),
            call("t3", "Bash", json!({ "command": "cargo test" })),
            result("t3", "exit status 101"),
            call("t4", "Bash", json!({ "command": "cargo test" })),
            result("t4", "exit status 0"),
            json!({ "kind": "sub.goal_finished", "ending": "done" }),
        ]);
        assert_eq!(facts.tool_calls, 4);
        let footer = facts.footer();
        assert!(footer.contains("files touched (1)"), "{footer}");
        assert!(footer.contains("commands run (2)"), "{footer}");
        assert!(footer.contains("cargo test → exit 101"), "{footer}");
        assert!(footer.contains("cargo test → exit 0"), "{footer}");
    }

    #[test]
    fn a_long_file_list_is_cut_and_says_it_was() {
        let mut records: Vec<Value> = (0..30)
            .map(|i| {
                call(
                    &format!("t{i}"),
                    "Read",
                    json!({ "file_path": format!("src/a{i}.rs") }),
                )
            })
            .collect();
        records.push(json!({ "kind": "sub.goal_finished", "ending": "done" }));
        let footer = Facts::from(&records).footer();
        assert!(footer.contains("files touched (30)"), "{footer}");
        assert!(footer.contains("and 18 more"), "{footer}");
    }

    #[test]
    fn a_refused_call_is_in_the_footer_because_the_parent_cannot_see_the_prompt() {
        // A subagent refused the write it needed and then reporting success has
        // to be catchable, and the parent is not the one who answered the
        // prompt.
        let facts = Facts::from(&[
            call(
                "t1",
                "Write",
                json!({ "file_path": "src/a.rs", "content": "x" }),
            ),
            json!({ "kind": "sub.denied", "id": "t1", "by": "user", "reason": "no" }),
            json!({ "kind": "sub.goal_finished", "ending": "done" }),
        ]);
        let footer = facts.footer();
        assert!(footer.contains("refused or blocked"), "{footer}");
        assert!(footer.contains("Write"), "{footer}");
    }

    /// **This test used to pass on a fixture the real tool never produces.**
    /// Every assertion below with no banner passed throughout the life of the
    /// defect, because `Bash` prepends its shell banner to *every* result and
    /// the parser read only the first line — so in production it returned
    /// `None` for every command a subagent ever ran, and the footer said "no
    /// result" every time. A fixture that agrees with its author is the exact
    /// failure this repository keeps paying for.
    /// A cut description names its cap, and a short one is left alone.
    ///
    /// **The string this guards is the only text the calling model has to pick
    /// a delegation target with.** A sentence that stops mid-clause with a bare
    /// ellipsis is indistinguishable from a short description, and 11 of 86
    /// real agent descriptions on this machine were over the cap when the row
    /// was filed.
    #[test]
    fn a_cut_agent_description_names_the_cap_and_a_short_one_is_untouched() {
        let long = "x".repeat(450);
        let cut = one_line(&long);
        assert!(cut.contains("300"), "the cap is not named: {cut}");
        assert!(cut.contains("450"), "the loss is not named: {cut}");
        assert!(
            cut.contains("No argument raises") || cut.contains("no argument raises"),
            "INV-011 requires a remedy or a plain statement there is none: {cut}"
        );

        // The positive control. A notice on every description is noise, and it
        // would also make the assertions above pass for the wrong reason.
        let short = "audits a crate for unwrap in library code";
        assert_eq!(
            one_line(short),
            short,
            "a description that fitted was annotated anyway"
        );

        // Whitespace is still collapsed, which is the function's other job.
        assert_eq!(one_line("two   words"), "two words");
    }

    #[test]
    fn an_exit_status_is_read_from_where_bash_actually_puts_it() {
        // The shape the real tool emits: banner first, status second.
        assert_eq!(
            exit_status("shell: posix — /bin/sh\nexit status 101\nerror output"),
            Some(101),
            "the banner is above the status on every real Bash result"
        );
        // And without one, for records older than the banner.
        assert_eq!(exit_status("exit status 0\nhello"), Some(0));
        assert_eq!(exit_status("exit status 101"), Some(101));
        // Not from prose that merely mentions one, which is what a model writes.
        assert_eq!(exit_status("the command exited with status 0"), None);
        assert_eq!(exit_status(""), None);
        // Not from far down a long output either: a line reading like a status
        // in the middle of a log is the command talking, not the harness.
        let buried = format!("banner\n{}\nexit status 7", "noise\n".repeat(10));
        assert_eq!(exit_status(&buried), None, "only the opening lines count");
    }
}

// endregion: Tests
