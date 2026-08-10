# The approval gate

A tool surface that is read-only by construction bounds the worst a rogue turn
can do to reading something it was entitled to read, and needs no gate to achieve
it. Emma writes files and runs commands, which inverts that property.
`crates/emma/src/approval.rs` is the thing standing in its place.

This document is about the gate's rules and the order they apply in, because the
order _is_ the design. See [the-loop.md](the-loop.md) for where the gate sits in
a turn, [harness.md](harness.md) for the hook that outranks it, and
[architecture.md](architecture.md) for why it reads facts from the tool rather
than from arguments.

## The rules, in order

### 1. A `PreToolUse` hook that denies wins

It is checked before any human is asked, and no answer overrides it — not a `y`,
not a session allowance, not `--dangerously-skip-permissions`. A hook is policy
the operator wrote down; the prompt is convenience for the person sitting there.
If a human could wave a hook through, the hook would be advice.

The check itself is in `agent.rs::run_tool_call`, which is where the hook runner
is, and it happens _before_ `approvals.request` is called. What `approval.rs`
enforces is that nothing in it can undo the denial: the gate never sees the call.
The model is told `"{reason} This is configured policy and cannot be approved
away."` `tests/loop.rs` pins it twice —
`a_hook_denial_overrides_every_approval` and
`a_hook_denial_outranks_a_network_grant`.

### 2. `reaches_network` — may bytes leave this machine

Asked **before** and **separately from** the write question, in `egress()`, which
is a separate function rather than another arm in `decide` precisely because it
is a separate question with its own grant, its own scope and its own prompt. It
returns `Allow` for every tool that does not declare egress, so the caller runs it
unconditionally and the network question cannot be skipped by an arm added above
it later.

The grant is **per host and lasts the session**. The first fetch to `docs.rs`
asks; every later fetch to `docs.rs` in this process does not; a fetch to
somewhere else asks again. `hosts_allowed` is a `HashSet<String>` on the
`Approvals` struct and dies with it.

### 3. `read_only` — may this damage the machine

`Read`, `Glob` and `Grep` run silently; `Write`, `Edit` and `Bash` ask. Prompting
for reads is how an agent becomes unusable in under a minute, and an unusable gate
gets turned off.

### 4. The prompt shows what will actually happen

The command, the diff, the path and size, the host and the URL or query. A prompt
the user cannot evaluate trains them to press `y`, which is worse than no prompt:
it manufactures consent and leaves a record saying they agreed.

## Why one boolean could not carry both

`ToolMeta::read_only` was once documented as meaning "cannot reach the network"
as well, and that reading was never true of the tools as they are. `WebFetch` and
`WebSearch` declare `read_only: true` and both talk to the outside — one of them by
driving a browser. The declaration is honest about what that bit asks, which is
_can this damage this machine_, and neither can: `WebFetch`'s throwaway Chrome
profile lives in the system temp directory and is removed on teardown, nothing is
written inside the working directory, no form is submitted
(`tools/web/src/fetch.rs`, `meta`).

Egress is a different risk with a different answer. **Writing is a risk the model
takes deliberately; egress is how a prompt-injected page turns a read tool into an
exfiltration channel.** A model that reads an attacker's page and then "searches"
for the contents of a `.env` has written nothing, destroyed nothing, and passes
every local-damage check on the way out. `WebSearch` is the tool that argument is
about most directly: a search is an arbitrary string the model chose, sent to a
third party, and it reads as a read.

Two fixes were considered and refused, and both are recorded at the code rather
than in a plan.

_Flip the web tools to `read_only: false`._ This is the obvious fix and it is the
same mistake wearing the other axis' clothes. It would fire a prompt on every page
read, which trains the operator to click through — and the operator who has learned
to hold down `y` holds it down through the `Bash` call that comes after. It costs
the gate on `Write` too.

_Prompt per network call._ Same failure by another route. A grant per URL is a
prompt per page.

So `ToolMeta` gained a second axis. `NetworkTarget::new` normalises the host —
trimmed, trailing dot removed, lowercased — so the gate's `HashSet` and the tool
agree on what "the same host" means: `Docs.RS.` and `docs.rs` are one grant. A
subdomain is deliberately a _different_ grant, because deciding `www.docs.rs` and
`docs.rs` are the same is a policy nobody asked for
(`tool-api::a_host_is_normalised_so_one_grant_covers_one_host`).

### The gate never parses arguments

`Approvals::request` takes `&dyn Tool` and asks it: `tool.meta()` and
`tool.network_target(args)`. It is the only place `network_target` is called in
the workspace. The gate knows that _some_ tools reach _some_ host and knows
nothing about how any of them spell it — `WebFetch` parses `args["url"]` in its
own crate, `WebSearch` derives the host from its `base_url` so a run pointed at a
stub is gated on the stub it will actually contact.

The alternative has been tried twice in this project's history in other forms — a
hard-coded `query` argument check, and inferring retrieval from an empty `chunks`
array — and both times it made "adding a tool is a crate plus one registry line"
false. The `NetworkTarget` doc in `tool-api` records both.

**A tool that declares `reaches_network: true` and answers `None` is denied.**
Fail closed: the gate cannot grant a destination nobody named, and the opposite
reading — "no target, so nothing to gate" — hands silent network access to whichever
tool forgets to implement one method. The denial says so in as many words, and
tells the model it is a defect in that tool rather than something to work around
(`a_tool_that_declares_egress_and_names_no_host_is_denied`).

### Two grants, two sets

`session_allowed` holds tool names; `hosts_allowed` holds hosts. They are separate
fields because the two grants are different shapes and must not stand in for each
other: a tool allowance covers one tool reaching anywhere, and a host allowance
covers one host reached by anything. Keying both on one set would mean approving
`WebFetch` once approved every host it might ever be pointed at, which is the
grant this design refuses to offer.

The `Question` enum exists so the wording on screen cannot drift from which set an
answer is filed under. On a network question, `a` grants no more than `y` does —
there is no "always, for any host" on offer, and the enum says so at the variant.

`a_grant_is_the_host_and_not_the_tool` and `a_second_host_is_a_second_question`
pin both directions with a one-answer queue, so a single `HashSet` doing both jobs
would go red.

## `-p`, and the bypass

`Gate` has exactly three members and there is no fourth.

**`Gate::Ask`** is the default and the only mode reachable without a flag.

**`Gate::Unattended`** is `-p` with no bypass. There is nobody to ask, so anything
needing approval is denied and the model is told why — including which host, on
the egress path. The two alternatives are both worse: proceeding silently makes
`-p` the way to get unattended writes without saying so, and prompting anyway
hangs on a question nobody can see, which in a CI job is a timeout an hour later
with no output explaining it. A read-only, non-networked tool is still never asked
about, even here.

**`Gate::SkipAll`** is `--dangerously-skip-permissions`. It exists because
scripting exists. It cannot be set from configuration or the environment, it is
announced loudly at startup by `main.rs` (`term.banner`, every run, before
anything happens), and every call it waves through is still written to the session
log. It is checked **first** in `decide`, before both questions, because reading
it once is one chance to get it wrong instead of two — and
`the_bypass_waves_egress_through_like_everything_else` exists because a bypass
that silently stopped covering a new axis would be a bypass that silently stopped
working for scripts.

The short `--yes` spelling means the same thing and is accepted **only alongside
`-p`** (`cli.rs`). Typing it at an interactive prompt must not be enough to turn
the gate off, because the whole hazard of a bypass is that it is easy to reach;
`--yes` at a terminal reads as "stop asking me" and means "run anything". The long
name is a sentence nobody types by accident. The error message names it.

## What is deliberately absent

There is **no persistent always-allow**, on either axis. "Yes, and stop asking for
this tool" and "yes, and stop asking for this host" both live in a `HashSet` on
`Approvals` and die with the process. A permission the user cannot see is a
permission they have forgotten they granted, and the place they would not see it
is a config file written six weeks ago. **The session scope is the longest scope a
permission may have, and there is no file anywhere in Emma that lengthens it.**

Empty input is **not** yes. `""`, `n` and `no` all deny; only `y`/`yes` allow and
`a`/`always` widen. A user who hits return to get their prompt back has not read
anything. Running out of input — end of stdin, or a scripted run out of answers —
denies, because silence is not consent
(`running_out_of_answers_denies`).

Every path that does not allow returns a `Verdict::Deny(String)` carrying a
sentence the model is told. A denial the model cannot read is a tool that
mysteriously does nothing. The sentences are specific about what not to do next:
declining a host says "do not try a different host to reach the same content", and
`declining_a_host_denies_and_tells_the_model_not_to_route_around_it` asserts it.

`Approvals::seen` records every decision and `decisions()` reads it back. Nothing
in the workspace calls it today; the struct doc says so, and the record a run
actually depends on is the session log, which the loop writes on every denial and
every call it lets through.

## The prompts

`preview(tool, args)` has one arm per tool that can change something, because a
generic JSON dump is exactly the prompt people learn to approve without reading:

- `Bash` → `$ <command>`, plus `in <cwd>` when the call named one.
- `Write` → the path, the byte count and the line count — the size rather than the
  bytes.
- `Edit` → the path, whether it is replacing every occurrence, and a `-`/`+` diff.

Anything without an arm falls back to pretty JSON — visibly worse, which is the
right pressure on whoever adds the next writing tool.

The `Edit` diff is **not computed and must not become one**. `Edit`'s arguments
_are_ the two sides, so showing them verbatim is the only rendering that cannot
disagree with what the tool will do. A minimised diff would be prettier and would
show the user a summary of the change rather than the change. It is cut at 40 lines
with an ellipsis so a large edit does not flood the terminal.

`network_preview(target)` is deliberately not a match on the tool name — the tool
already composed the only part that varies. Both of its lines are load-bearing and
neither is enough alone. The host is what is being granted, and granted for the
rest of the session, so it has to be the thing the eye lands on. The detail is the
errand — the URL, the query — and it is the half that distinguishes the fetch the
user asked for from the fetch a page asked for. A prompt with only the host cannot
tell a search for a crate name from a search for the contents of a file, which is
what `the_network_prompt_shows_the_host_and_the_errand` asserts.

## The `EXEMPT` list

```rust
const EXEMPT: &[&str] = &["TaskCreate", "TaskUpdate"];
```

This is a hole in the gate, written as a list so that it is findable and deletable
rather than buried in a condition, and given its own section at the top of the
file so a reviewer trips over it.

The argument. Those two tools rewrite `.emma/tasks/tasks.md`, and they declare
`read_only: false` honestly — they were deliberately _not_ flipped to `true` to
dodge the gate, which is correct, because a tool that misreports itself leaves the
gate protecting nothing in general and not just for that tool
(`tools/tasks/src/lib.rs::only_the_readers_claim_read_only` pins which two claim
it). But the write is confined to the tool's own bookkeeping file under `.emma/`,
and it is the write a model performs several times per goal. This file's own
design says a prompt the user cannot evaluate is worse than no prompt because it
manufactures consent — and "TaskUpdate wants to tick a checkbox, allow?" fifteen
times in one goal is precisely the prompt that teaches somebody to hold down `y`,
including through the `Bash` call that comes after it. **Exempting these two
protects the prompts that matter.**

Two things make it survivable: a `PreToolUse` hook denial is resolved before
approvals are consulted, so an operator can still block them; and every call,
exempt or not, is written to the session log.

The missing thing is an axis on `ToolMeta` for the _scope_ of a write, not merely
its existence. That belongs in `tool-api`, and when it lands this list should be
deleted — the tools will classify themselves. `ToolMeta` has since gained a second
axis and it is **not that one**: `reaches_network` splits egress out of
`read_only` and does nothing for these two.

The test written about the list is written about what is _not_ on it. `Bash`,
`Write` and `Edit` are asserted absent and asserted gated, because the hazard is a
future edit quietly adding a tool that writes the user's source tree
(`the_exemption_covers_the_task_writers_and_nothing_that_touches_the_tree`).

## What the gate does not cover

None of these are oversights, and all of them are recorded at the code.

**What comes back.** Rule 2 gates where bytes go and nothing else. An approved
host is a destination the user chose, not a source they trust. The page that
arrives is still attacker-controlled text entering the model's context, and
nothing in this file or anywhere else reads it. That is the single most important
sentence in this document.

**Incidental network access.** `Bash` can `curl` and declares
`reaches_network: false`. It is not exempt: `read_only: false` already means every
`Bash` call is shown to a human as the command itself, so its egress is approved
at the same moment and with strictly more information than a host name. Declaring
`true` would oblige `network_target` to name a destination, and naming one means
parsing shell to find it — an arms race with every quoting trick there is, losing
quietly (`tools/fs/src/bash.rs`, `meta`).

**Enforcement.** Both axes are declarations. Nothing stops a lying tool from
opening a socket or writing a file. What keeps them honest is that the tool crates
own the tests: `tools/fs/tests/read_only.rs` runs every `read_only` tool against a
populated sandbox and asserts the tree is unchanged, and pins which tools claim it;
`tools/tasks/tests/tools.rs` does the same. The gate protects a claim, and the
claim is checked somewhere else.

**Prompt injection deciding the destination.** The model chooses the host, and the
human approves it. A convincing page can persuade a model to ask for a host the
human then approves because it looks plausible. The per-host grant narrows this —
it cannot be widened to "any host", and it dies with the process — but it does not
close it. The prompt showing the errand alongside the host is the only mitigation
that operates at the moment of the decision, which is why it is a rule rather than
a nicety.

**A tool built with the wrong shared state.** Outside this file, but it lands
here: `Write`'s refusal to clobber an unread file depends on sharing a
`ReadTracker` with `Read`, which the type system does not enforce. See
[architecture.md](architecture.md).
