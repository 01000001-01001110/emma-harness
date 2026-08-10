# Architecture

Emma is one binary built from seven crates. This document is about where the
seams are and which of them are load-bearing — what each crate owns, which way
the dependencies point, and which boundaries exist because something went wrong
when they were not there. Anything you can learn by reading one file is not
here; `cargo doc` and the module docs cover that better and stay current.

Three companion documents pick up where this one stops: [the-loop.md](the-loop.md)
for one goal end to end, [harness.md](harness.md) for the configuration
directory, and [approval.md](approval.md) for the gate.

## The crates

```text
                            crates/emma
              the loop · the gate · the CLI · the terminal
     ┌──────────────┬──────────┬───────────────┬──────────────────┐
     ▼              ▼          ▼               ▼                  ▼
 emma-harness   emma-llm  emma-tools-fs  emma-tools-tasks   emma-tools-web
  .emma/,       Messages   Read Write     TaskCreate/Get/    WebFetch,
  personas,     API over    Edit Glob      List/Update       WebSearch
  skills,       raw HTTP    Grep Bash            │                │
  commands,        ▲            ▲                │                │
  hooks            │            └────────────────┘                │
     │             │             path::resolve —                  │
     │             │             one containment impl             │
     │             └────────────────────────────────────────────  ┘
     │                        ApiKey, home_dir —
     │                        one rule for where a key may live
     │                                                    │
     └──────────────► emma-tool-api ◄────────────────────  ┘
             Tool · ToolMeta · ToolError · Registry
                (depends on nothing in this workspace)
```

Read the arrows as "depends on". `crates/emma` depends on all five; every tool
crate and the harness depend on `emma-tool-api`; `emma-tools-tasks` depends on
`emma-tools-fs` for the containment check and nothing else; `emma-tools-web`
depends on `emma-llm` for `ApiKey` and `home_dir` and nothing else.

Every arrow points down and none point back up. `emma-tool-api` depends on
nothing else in the workspace (`crates/tool-api/Cargo.toml`), which is what
makes it usable as the vocabulary every other crate speaks.

### `crates/tool-api` — the contract

One trait, one registry, and a taxonomy of failure. `Tool` (`lib.rs`) asks a
tool for its name, its description, its JSON schema, its `ToolMeta`, an optional
`network_target`, a cheap synchronous `validate_args`, and an async `invoke`
whose signature is `Result<Result<ToolOutcome, ToolError>>` — the outer result
for faults that end the turn, the inner for failures the model should see and
route around.

The governing rule is stated in the module doc and is worth repeating because
everything downstream assumes it: **a `ToolError` is a fact about the call, never
about what the world contains.** A read of an empty file succeeds. A glob
matching nothing succeeds. That rule did not survive first contact with `Bash`,
and the resolution is recorded in the same doc: the line is whether the command
_ran_, so could-not-spawn / timed-out / killed are `Failed` and anything that
started and finished is `Ok` with `exit status <n>` as the first line of the
content (`tools/fs/src/bash.rs`). It was explicitly not resolved as "Bash is the
documented exception".

`ToolMeta` carries three declarations. `read_only` and `reaches_network` are both
read by the approval gate; `idempotent` is read by nothing today, and the struct
doc says so rather than letting a reader assume a mechanism exists. There is
deliberately no `Default` impl, so adding a field breaks every construction site
in the workspace instead of silently granting a safety property to tools written
afterwards.

`Registry` holds `Arc<dyn Tool>`, looks tools up by the name the model used, and
renders the surface twice — `wire_definitions()` for the provider and
`schema_hash()`, a 16-hex-character digest over every byte the model is shown, so
a description change is attributable rather than invisible.

### `crates/harness` — configuration, resolved

Discovery, `config.json`, personas, skills, commands, and hooks. The whole crate
exists to turn a directory on disk into one struct, `Harness`
(`crates/harness/src/lib.rs`), whose fields are already decided: `instructions`
is a `String`, not a loader; `persona` is a resolved `Option<String>`; `skills`,
`commands` and `hooks` are private and reached through accessors. `hooks.rs` is
split out because it is not "file reads and a serde struct" — it is a subprocess
supervisor, and the two files fail for different reasons. [harness.md](harness.md)
covers the layout, discovery and hook security in full.

The only reason this crate depends on `emma-tool-api` at all is
`Harness::select_tools`, which consumes a `Registry` and returns a filtered one.

### `crates/llm` — the Anthropic client

`Request` in, `AssistantTurn` out, behind the `Provider` trait
(`crates/llm/src/lib.rs`). There is no official Anthropic SDK for Rust, so
`anthropic.rs` speaks the Messages API over raw HTTP; `auth.rs` resolves a key
from `ANTHROPIC_API_KEY` or `~/.emma/credentials.json`; `retry.rs` handles the
retryable classes.

Two things here shape everything above them. `Request`'s field order —
instructions, tools, history, query — _is_ the cache prefix order, expressed as a
struct so that putting a per-turn byte at position zero is not something the type
lets you write. And `Usage::input_tokens` is not the input: with caching in play
it carries only the uncached remainder, so anything folding a budget must use
`billable_total_tokens()`. The type doc records the measurement that taught this
(a 72,549-token turn reported as `input_tokens: 2`) and deliberately omits a
`total()` method so the wrong sum cannot be written.

`LlmError`'s redaction guarantee lives in its `Display` and `Debug` impls rather
than at each construction site, because construction sites are where it kept
failing. What that does not cover is written down in the same place: reading a
`pub` field directly, a key this process never wrapped in `ApiKey`, and a gateway
that re-encodes the key before echoing it.

The crate has no runtime dependency on `emma-tool-api`; the dev-dependency exists
only so one test can prove the seam — a real `Registry`'s `wire_definitions()`
goes out on the wire and a `tool_use` block comes back naming a tool the registry
can look up.

### `crates/emma` — the loop, the gate, the CLI

The only place the other crates meet, and therefore the only place the properties
that span them can be enforced. It is a library plus a thin binary, and that is
not packaging taste: the loop cannot be tested without a public API, so `main.rs`
is argument parsing and wiring while everything with a decision in it lives in
the lib, where a scripted `Provider` drives it (`crates/emma/tests/loop.rs`).

- `agent.rs` — the loop. See [the-loop.md](the-loop.md).
- `approval.rs` — the gate. See [approval.md](approval.md).
- `goal.rs` — the goal text, the `DoneCheck` trait, and the kick.
- `cli.rs` — a hand-rolled parser, chosen because the one cross-flag rule that
  matters (`--yes` only alongside `-p`) would be a runtime check under a derive
  parser anyway.
- `commands.rs` — `emma api`, `emma model`, `emma config check`: the three things
  that run without a model call.
- `settings.rs` — `~/.emma/settings.json`, the personal model preference,
  deliberately _not_ the harness.
- `session.rs` — the JSONL transcript and the fold that turns it back into a
  message list.
- `skill.rs` — the `Skill` tool, which lives here rather than in `tools/fs`
  because it is the one tool whose content comes from the harness.
- `term.rs` — line-oriented output, no TUI framework, so the agent never owns
  scrollback or fights the programs it shells out to.

### The tool crates

`tools/fs` is `Read`, `Write`, `Edit`, `Glob`, `Grep`, `Bash`. `tools/tasks` is
`TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate` over a markdown file at
`.emma/tasks/tasks.md` (`tools/tasks/src/doc.rs`, `RELATIVE_PATH`). `tools/web`
is `WebFetch` and `WebSearch`, the first driving a real Chrome over CDP and the
second calling Brave.

Every name is Claude Code's, exactly. That is a compatibility contract, not a
naming preference: a hook matcher or an allow-list written for one program works
for the other, and each crate pins the spelling in a test
(`tools/fs/src/lib.rs::the_surface_is_the_six_claude_code_names`, and the
equivalents in `tools/tasks` and `tools/web`).

## The boundaries that are load-bearing

### Configuration resolves before the loop starts

This is the one that matters most, and the one worth defending in review.

`Harness::boot()` runs at the top of `run()` in `main.rs`, before a registry
exists and before a provider is built. From then on the loop holds a `&Harness`
and touches exactly four things on it: `harness.instructions`,
`instructions_hash()`, and `run_hooks()` at the two dispatch sites. `agent.rs`
never opens a file, never learns that a persona was selected, never learns which
layer a sentence came from, and never branches on configuration. The empty
harness — a `.emma/` that exists and contains nothing — needs no special case
anywhere in the loop, and that is the proof the harness is separable from the
engine.

The harness crate is shaped to keep it that way. The instructions are a `String`
and not a loader. The hook payload builder takes a `HookCall` and **not** a
`&ToolOutcome` (`crates/harness/src/hooks.rs`), so plumbing an internal field
through to an operator-authored external process requires a signature change a
reviewer will see. `select_tools` takes the `Registry` **by value**, so the
caller cannot keep the unfiltered one around by accident — which is the entire
failure that method exists to prevent.

### The gate reads facts from the tool, never from arguments

`Approvals::request` takes `&dyn Tool` rather than a name and a bool, and calls
`tool.meta()` and `tool.network_target(args)` itself (`crates/emma/src/approval.rs`).
It is the only caller of `network_target` in the workspace. The alternative —
the gate reaching into `args["url"]` — has been tried twice in this project's
history in other forms, and both times it made "adding a tool is a crate plus one
registry line" quietly false. The `NetworkTarget` doc records both. See
[approval.md](approval.md).

### One containment implementation

`tools/tasks` depends on `emma-tools-fs` for `path::{root, resolve, display}` and
nothing else, and its `Cargo.toml` says why: containment must have exactly one
implementation in this workspace, because a second one is a second thing to get
wrong and the two would disagree in precisely the cases that matter. Even
`open_count`, which is a plain function and not a tool call, builds a `ToolCtx`
and goes through `store::tasks_path` so the same check applies
(`tools/tasks/src/lib.rs`).

### The provider boundary leaks, and the leak is documented

`Provider` is type-clean and the Anthropic wire shape still escapes it in exactly
two places, both in `agent.rs`: the loop builds its own
`{"type":"tool_result","tool_use_id":…}` blocks, and it pushes
`AssistantTurn::raw_content` back verbatim. A second provider is therefore a
refactor of those two sites plus the passthrough, not a new file beside
`anthropic.rs` — OpenAI puts each result in its own `role:"tool"` message. The
refactor must not delete `raw_content`: thinking-block signatures do not survive
reassembly, so a turn rebuilt from `text` + `tool_calls` makes the _next_ call
fail. Both `crates/llm/src/lib.rs` and `agent.rs` say this at the site.

### Registration is a decision, made once, at startup

`main.rs` builds the registry in a fixed order: the six filesystem tools from
`fs_tools()` (which also wires the one shared `ReadTracker` that makes `Write`'s
refusal-to-clobber meaningful), the four task tools unconditionally, `Skill` only
if `harness.skill_catalog()` returned `Some`, and the web tools only as
`web_tools()` reports them available. `web_tools()` resolves Chrome and the Brave
key _first_ and returns a `WebSurface { tools, skipped }`; the skipped lines are
printed to the terminal and by `emma config check`. The rule behind all of it:
an absent tool is one the model can reason about, and a present-but-broken one is
a trap with a description attached.

`harness.select_tools(registry)` is then applied, and it is a **real filter**. In
the predecessor system the same field validated that each named tool existed and
removed nothing. That was survivable when every tool was a read-only search; an
operator who writes `"tools": ["Read", "Grep"]` and gets `Bash` anyway has been
handed a permission boundary that is a comment, at the one place where the
failure costs them their working tree. The startup assertion is kept as well, so
a typo is a load error rather than a silent narrowing.

### What is _not_ a boundary

Session state. `ToolCtx` carries `cwd`, `session_id` and `turn_id` and has
nowhere to put a tracker, so the read-tracking that `Write` depends on lives in
the tool structs behind an `Arc`. "These three tools share one tracker" is a
wiring convention enforced by `fs_tools()` being the only supported constructor,
not something the type system holds — a `Write` built with the wrong tracker
refuses nothing and it compiles. This is recorded as a known defect on `ToolCtx`
in `tool-api` and again in `tools/fs/src/lib.rs`, cheaper to fix at six tools
than at sixteen.

## Where a change lands

| You want to change…                                | It lives in                                                             |
| -------------------------------------------------- | ----------------------------------------------------------------------- |
| what a tool may do, or how failure is expressed    | `crates/tool-api`                                                       |
| what the model is told before it starts            | `crates/harness` (personas), or `crates/emma/src/goal.rs` (the opening) |
| how a goal ends                                    | `crates/emma/src/goal.rs`, `DoneCheck`                                  |
| what is asked of a human                           | `crates/emma/src/approval.rs`                                           |
| what runs at the highest-privilege point of a turn | `crates/harness/src/hooks.rs`                                           |
| what is written down for a resume                  | `crates/emma/src/session.rs`                                            |
| the wire                                           | `crates/llm/src/anthropic.rs`, plus the two sites in `agent.rs`         |
