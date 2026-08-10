# The loop

One goal, end to end, and then the properties that shape it. Everything here is
`crates/emma/src/agent.rs` unless another file is named; the companion documents
are [architecture.md](architecture.md), [harness.md](harness.md) and
[approval.md](approval.md).

The loop is what separates Emma from a chat client. A chat client stops when the
model stops. Emma holds a goal: when the model stops without claiming completion,
the loop tells it so and asks again, until the goal is claimed done or a budget
is spent.

## One goal, end to end

```text
run_goal(goal)
  │
  ├─ log "goal"  ─ text, opening, instructions_hash, tool_schema_hash,
  │                model, done_check
  │
  ├─ query = [ Message::user(goal.opening(done)) ]
  │
  └─ loop:
       ├─ interrupt tripped?          → Ending::Interrupted
       ├─ elapsed > wall_clock?       → Ending::Deadline
       ├─ iterations >= max?          → Ending::Iterations
       │
       ├─ call_model(Request { instructions, tools, history, query })
       │     └─ select! on { interrupt, event stream, the send future }
       │
       ├─ iterations += 1
       ├─ tokens += turn.usage.billable_total_tokens()
       ├─ log "model_call"   ─ all four usage fields plus the running total
       ├─ log "assistant"    ─ text and raw_content
       ├─ tokens > max?               → Ending::Tokens
       │
       ├─ no tool calls?
       │     ├─ done.verdict(goal, text)  → Done::Yes → Ending::Done
       │     ├─ no tool calls since last kick, and kicks > 0 → Ending::Stalled
       │     ├─ kicks >= max_kicks                          → Ending::KicksExhausted
       │     └─ kicks += 1; push assistant(raw_content); push user(kick_text)
       │
       └─ has tool calls:
             push assistant(raw_content)          ← verbatim, never rebuilt
             for each call:
               ├─ registry.get(name)?             → failure block, no_such_tool
               ├─ tool.validate_args(args)?       → failure block
               ├─ in failed_now?                  → failure block, already_failed
               ├─ log "tool_call"
               ├─ harness.run_hooks(PreToolUse)   → denied? failure block
               ├─ approvals.request(tool, args)   → Deny? failure block
               ├─ tool.invoke(ctx, args)
               │     ├─ Ok(Err(ToolError))        → failure block + PostToolUse
               │     └─ Err(fault)                → failure block
               ├─ truncated? append the warning
               ├─ harness.run_hooks(PostToolUse)  → append any context
               └─ log "tool_result" (the block, not the string)
             push user(tool_results)
```

Then `goal_finished` is logged with the ending, the tokens, the iterations, the
kicks and the elapsed time; the goal and the assistant's last text are collapsed
into `history` — whatever the ending, because a run that hit its token budget
still happened and the next goal must not be told a story in which it did not.

### The opening message

`Goal::opening(&dyn DoneCheck)` (`goal.rs`) composes the goal text with whatever
the active done-check needs the model to know. The contract comes from the check
rather than from the goal, because a contract describing a rule the loop is not
applying teaches the model a completion ritual that decides nothing —
`goal.rs::the_opening_carries_the_contract_of_the_check_actually_in_force` pins
that. It is stated once, at the start, rather than repeated in every kick: it
sits in `query`, after the cached prefix, and re-sending it each iteration would
be the same bytes at a different offset every time.

The composed string is logged on the `goal` record verbatim rather than
re-derived later, because a fold that re-derived it would need a name-to-`impl`
table kept in step with `goal.rs` forever.

### The model call

`call_model` is a `tokio::select!` over three things, `biased` in that order: the
interrupt, the event channel, and the send future. The `if !closed` guard on the
receive arm is load-bearing — a closed channel is permanently ready and would
starve the branch below it. `Ok(None)` means the user interrupted; the caller
turns that into `Ending::Interrupted`.

The interrupt is both a flag and a `Notify` (`Interrupt`, same file). The flag is
what an iteration boundary tests; the notification is what cuts a call that is
thirty seconds into a sixty-second answer. It uses `notify_one` rather than
`notify_waiters` so a signal arriving in the gap between two waits still lands.

### One tool call

`run_tool_call` returns three things: the result block, whether the tool actually
ran and succeeded, and a label for the kick when it did not. Six things can stop
a call before `invoke`, and **every one of them produces a `tool_result` block
with `is_error: true`** rather than an abort: unknown tool, failed
`validate_args`, the memo, a `PreToolUse` denial, a refused approval, and — after
the fact — a `ToolError` or an outer fault.

The order of the last two checks is deliberate and stated in `approval.rs` rule 1:
the hook runs _before_ the human is asked. A gate a human can wave through is
advice, so a `PreToolUse` denial is resolved before anybody gets a vote on it, and
the model is told "This is configured policy and cannot be approved away."

### The result block

On success, `content` is `outcome.content`, plus a truncation warning if
`outcome.truncated`, plus any `context` a `PostToolUse` hook asked to append. A
hook can annotate a result; it can never rewrite one — `HookVerdict::context` is
a `Vec<String>` that the loop appends, and there is no field for replacing.

The `tool_result` log record carries the **block**, not the rendered string. The
block also carries the `tool_use_id` that pairs it with its call, and `is_error`
when it failed; a string alone cannot be turned back into a message. Failure
blocks are logged under the same kind as successful ones for the same reason —
`session::fold` rebuilds the user turn from these, and a fold that saw only the
calls that worked would reconstruct a turn the API rejects.

### The budget fold

`tokens += turn.usage.billable_total_tokens()` and the `model_call` record are
both written **before** `if tokens > max_tokens`. That ordering is the whole
point: in the predecessor the counter only reached the log on a completed turn,
which made aborting the cheapest way to spend money. A run that dies on the token
cap is now recorded having spent exactly what it spent.

`billable_total_tokens()` and never the bare `input_tokens`: with prompt caching
in play the latter reports only the uncached remainder and under-counts a cached
turn by up to ~10×, which would silently loosen the cap folded from it
(`crates/llm/src/lib.rs`, `Usage`).

There are three budgets plus the kick count because they fail differently. A
model calling one cheap tool forever is caught by iterations and not tokens. A
model reading enormous files is caught by tokens and not iterations. A tool
waiting on a network that will never answer is caught by neither, which is what
the wall clock is for. Defaults: 60 iterations, 500,000 tokens, 30 minutes, 3
kicks (`Budgets::default`).

### The kick

When the model stops with no tool calls, `done.verdict(goal, &turn.text)` is
consulted. `Done::Yes` ends the goal. `Done::No(why)` composes a kick from the
reason, the restated goal, and up to five things that have already failed
(`goal::kick`, `tail(&failed_ever, 5)`). That last part matters: without it the
honest reading of "continue" is "try again", and the first thing a model retries
is the thing that just failed.

The `kick` log record carries both `why` and the composed `text`, because they
differ — `why` is the reason a human reads, and `text` is what the model was
actually sent, which is what the fold has to put back.

### Endings

Every ending is reported to the user in words and names the limit that fired,
because "I stopped" without a reason is indistinguishable from a crash. There is
a test asserting that every budget ending's message contains a digit
(`every_ending_says_which_limit_fired`). The variants are `Done`,
`KicksExhausted`, `Stalled`, `Iterations`, `Tokens`, `Deadline`, `Interrupted`,
and `Provider(String)` for a provider failure retrying will not fix.

## The properties, and what each one cost

### A tool failure is a `tool_result`, never an abort

Every failure class becomes a block with `is_error: true` and the loop continues.
This was measured in the predecessor system: five of eight failure classes were
ending turns silently, the user saw "Something went wrong on my side", and the
model never learned anything had failed. Emma's tool surface is larger and fails
more often, so the lesson applies harder.

Even the outer `Result` — a tool claiming the session cannot continue — is not
allowed to abort silently. It becomes a `tool_fault` result the model can read,
and the loop's own budgets remain the only thing that ends a goal.

The failure block carries `is_error` _and_ a typed body:
`{"kind":…, "tool":…, "detail":…}`. The flag is what the API and the model's own
training key on; the body says which failure it was and what to do instead
(`failure_block`, and `a_failure_block_is_flagged_and_typed`).

### A failed call is not repeated until something else has succeeded

Not "never again this turn". That is affordable only when the whole tool surface
is one read-only search. Emma's working rhythm is _run the tests, see them fail,
fix a file, run the tests again_ — and a memo keyed on the call alone would forbid
the second run, which is the one that proves the fix.

So `failed_now` is a `HashSet<String>` keyed on `memo_key(call)` — the tool name
plus its canonically rendered arguments — and it is **cleared the moment any tool
succeeds**. An identical retry is refused only when literally nothing has changed
since it failed, which is the case where retrying is a loop rather than a step.
`crates/emma/tests/loop.rs` pins both directions:
`a_failed_call_is_not_repeated_with_the_same_arguments` and
`a_failed_call_may_be_repeated_once_something_else_has_succeeded`.

That rule also absorbed the `Bash` contract change without an edit. A non-zero
exit used to be `ToolError::Failed`, so `grep -q` answering "no" read as a broken
shell and would have poisoned `Bash` for the rest of the turn; it is now `Ok`
with `exit status <n>` in the content. Nothing in `agent.rs` moved, because
nothing here branches on a tool's identity, on `ToolError::kind`, or on exit
status. **Exit status is not a failure signal anywhere in this file** — `ToolError`
is the only one, and adding a second would re-introduce the bug the contract
change removed.

### `raw_content` is echoed back verbatim

The assistant message pushed into `query` is `turn.raw_content` — the provider's
own content array, including thinking blocks, unedited. Reassembling it from
`text` and `tool_calls` invalidates thinking-block signatures and the _next_ call
is rejected. This is why the session log stores `raw_content` as well
(`session.rs`), why the fold drops a turn that has none rather than approximating
it, and why the note at the `tool_result` construction site says a second-provider
refactor must not delete the field.

`text` is stored beside it anyway, duplicating bytes on purpose: the session file
is an audit trail somebody reads with `grep`, where one plain line beats a JSON
array of escaped blocks, and the fold reads `text` to collapse a finished goal
into the one-line answer `history` carries between goals.

`the_assistant_turn_is_echoed_back_exactly_as_it_arrived` in `tests/loop.rs` is
the guard.

### Usage is recorded before the budget is tested

Covered above under the budget fold. Stated separately here because it is the one
property with no visible symptom when it breaks: the run still stops, the numbers
are just wrong in the direction that flatters the run.

## The kick's two bounds, and the honest limit

The kick is bounded twice, and the goal ends the moment either bound is reached:

- `max_kicks` — a hard count for the whole goal, default 3, `--max-kicks` on the
  command line. Reaching it is `Ending::KicksExhausted`.
- No two kicks in a row without tool use between them. `tool_calls_since_kick` is
  reset when a kick is sent and incremented for every tool call; if the model
  stops again having called nothing, that is `Ending::Stalled`. It has answered
  the kick, and asking a third time is the loop arguing with itself.
  (`a_model_that_stops_twice_without_touching_a_tool_is_believed`.)

Both sit under the iteration, token and wall-clock budgets, so the worst case is
bounded by arithmetic rather than by good behaviour. The cost is that a goal
genuinely needing a fourth nudge stops one step early — and says which limit
stopped it, so the user can raise it.

### What done-detection actually guarantees

`goal.rs` argues through four candidate authorities and builds the smallest.

_The loop stops when the model stops_ costs nothing and catches nothing — a model
that runs one command, reads the first error and writes "this looks like a
session-API mismatch" has stopped, and the goal is untouched. That is the
behaviour Emma exists to not have.

_A check command exits zero_ touches reality and passes vacuously: `cargo test`
on a target with no tests, a `test -f` against a path that was already there, a
suite whose failing case was deleted rather than fixed. It also cannot express
goals with no mechanical criterion, which is most of the goals a person types.

_No open tasks remain_ — `emma_tools_tasks::open_count`, which answers without a
tool call — is the most legible, because the user can watch the file in an editor
while it happens, and strictly better than prose because the agent had to write
the claims down first. Legible is not honest: a model that learns closing tasks
ends the loop will close them, and a model that never opened a task has an empty
list from turn zero, which reads as _done_ before it has started.

_The model declares done_ is what is implemented, as `MarkerClaim`. The model must
end its final message with the line `GOAL COMPLETE`. `claims_done` is tolerant
about decoration — `**GOAL COMPLETE**` means the same thing and spending a kick on
formatting would be silly — and intolerant about position, so a sentence _about_
the marker ("I will print GOAL COMPLETE when the tests pass") is not a claim.

`DoneCheck` is a trait rather than an `if`, and it is `async`, because every
alternative reads a file or runs a process. The loop knows only the trait: it
asks for a verdict and composes a kick from whatever reason comes back, so
swapping in a task-list check or a check command is a different value in
`Setup::done`, not surgery here. The intended upgrade — recorded in `goal.rs`,
not built — is to use `open_count` to decide when to **stop asking** and gate the
verdict behind something the model does not author.

**The honest statement of the guarantee is "the loop will not stop _before_ the
model says it is finished", not "the loop stops when the work is finished".**
Every candidate above moves that line and none of them erases it, because each is
ultimately a signal the model itself produces. `goal.rs` says this in those words;
it is repeated here because it is the single thing about Emma most likely to be
overstated by someone summarising it.

## What is written down

Every record kind the loop appends is listed in `session.rs`: `goal`,
`model_call`, `assistant`, `kick`, `tool_call`, `tool_unknown`, `denied`,
`tool_failed`, `tool_fault`, `tool_result`, `hook`, `goal_finished`. The format is
one JSON object per line, chosen because a truncated final line is the only damage
a crash can do and it is skipped on read (`a_torn_final_line_costs_only_that_line`).

`session::fold` turns a file back into the message list that was **sent** — not a
tidied reconstruction of the conversation — and the loop's records were designed
so that it can. A turn whose `tool_use` blocks were not all answered is dropped
whole, because the API rejects both an unanswered call and an unmatched result.
`session.rs` also states plainly what is _not_ built: there is no `--resume`
command, nothing here decides what a resumed run should send, and nothing
addresses exactly-once tool side effects across a crash.
