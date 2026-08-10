# Emma

A small agent you run from a terminal. You type `emma`, it reads its
configuration from the directory you are standing in, and it works on your
goal until the goal is met or it runs out of the budget you gave it.

It is not a chatbot with tools bolted on. It is a loop with a goal, a set of
files that tell it who it is, and a filesystem it can actually touch.

```
$ emma
> port the auth middleware to the new session API and make the tests pass
```

## What it is

**A loop.** Send the conversation to a model, run whatever tools it asks for,
append what happened, send it again. Stop when the goal is met, when the
budget is spent, or when a rule says stop. Everything else in this repository
exists to serve that loop or to keep it honest.

**A harness.** Emma has no prompt compiled into it. It discovers a
configuration directory by walking up from your working directory, the way git
finds `.git`, and loads a persona, its skills, its commands and its hooks from
files you wrote. Swapping the directory swaps the agent. The same binary is a
coding agent, a research agent, or an agent that knows your ClickUp workflow,
depending on nothing but which files it found.

**A filesystem tool surface.** Read, write, edit, glob, grep, run a command,
keep a task list, read a web page. The things a person does at a terminal,
available to a model that has a reason to do them.

**A goal, held.** Most agents answer and stop. Emma is told what "done" looks
like and keeps going — a tool failed, so try the other approach; the tests are
still red, so read the failure and fix it — until done is true or it runs out
of room. That is the difference between something that answers and something
that finishes.

## What it is not

- Not a service. No web UI, no HTTP server, no accounts, no login.
- Not multi-user. One person, one terminal. Permissions between colleagues are
  a different product's problem.
- Not a retrieval engine. There is no corpus, no index, no chunking, no
  citation enforcement. If Emma needs to know what a file says, it reads the
  file.

## Start here

```
$ cargo build --release          # or take a binary from a tagged release
$ emma api                       # paste your Anthropic key; it is not echoed
$ emma init                      # write a working .emma/ in this directory
$ emma "make the tests pass"
```

Already using Claude Code in this repository? Emma reads `.claude/` as it
stands — you can skip `init`. See **[docs/getting-started.md]** for the full
walkthrough, including what transfers from `.claude/` and what does not.

## What it does when you are not watching

Emma writes files and runs commands, so the interesting question is what it
does without asking.

Read, Glob and Grep run silently. Write, Edit and Bash ask, and the prompt
shows what will actually happen — the command, the diff, or the path and size.
Reaching a new host asks once for that host. Answering `a` allows one tool for
the rest of the process and no longer: there is no permission that outlives
the run, and none that can be set from a config file. A `PreToolUse` hook that
denies cannot be approved away by anyone at the keyboard.

`-p` is the non-interactive mode: there is nobody to ask, so anything needing
approval is denied and the model is told why.
`--dangerously-skip-permissions` turns the gate off, announces itself on every
run, and cannot be set from configuration.

The reasoning behind each of those is in **[docs/approval.md]**.

## Configuration, in one screen

```
.emma/
  config.json              the spine: which persona is default, what hooks exist
  personas/
    _shared/rules.md       rules every persona obeys
    _shared/business.md    context every persona has
    <name>/rules.md        this persona's rules
    <name>/soul.md         this persona's voice
  skills/<name>/SKILL.md   a capability, described in markdown
  commands/<name>.md       expanded when you type /<name>
  hooks/<script>           run before or after a tool call
  tasks/tasks.md           the agent's task list, in a file you can edit
```

`emma init` writes the smallest version of this that works. `emma config check`
loads it and prints exactly what the model will be told, without calling a
model — it is the first thing to run when Emma does something you did not
expect.

Emma also reads `.claude/` when it finds one, so configuration written for
Claude Code works without being rewritten. `.emma/` wins outright when both
exist; nothing is merged.

## The model

The Anthropic API, directly. Emma owns its loop, so it needs a model it can
drive rather than a runtime that drives itself — the `claude` CLI and the
Agent SDK were investigated and dropped for that reason.

One provider today. A second is a refactor rather than a new file, because the
loop still builds Anthropic-shaped tool results itself.

## What it cannot do yet

Stated plainly, because the alternative is you finding out.

- **Done is the model's own claim.** Emma stops when the model says the goal is
  met. The honest guarantee is that the loop will not stop _before_ that, not
  that the work is finished. Checking a goal against the project's own tests is
  designed and not built.
- **Resume brings back the conversation, not the work.** `--resume` restores
  the transcript and the budget it already spent; it re-runs nothing, and you
  type a new goal at it.
- **No `emma sessions`.** Nothing lists what you could resume.
- **The web tools read.** `WebFetch` returns a page as markdown; nothing clicks,
  types, fills or submits. That code exists and is deliberately unreachable.

## Documentation

|                           |                                                            |
| ------------------------- | ---------------------------------------------------------- |
| [docs/getting-started.md] | install, key, `init`, and `.claude/` compatibility         |
| [docs/architecture.md]    | the crates, what each owns, and the boundaries that matter |
| [docs/the-loop.md]        | one goal end to end, and the properties that were paid for |
| [docs/harness.md]         | `.emma/`, personas, skills, commands, hooks, discovery     |
| [docs/approval.md]        | the gate, both of its axes, and what it does not cover     |

[docs/getting-started.md]: docs/getting-started.md
[docs/architecture.md]: docs/architecture.md
[docs/the-loop.md]: docs/the-loop.md
[docs/harness.md]: docs/harness.md
[docs/approval.md]: docs/approval.md
