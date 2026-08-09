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

**A filesystem tool surface.** Read, write, edit, glob, grep, run a command.
The things a person does at a terminal, available to a model that has a reason
to do them.

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

Each of those was built and shipped in [tustle-agent], and each is
deliberately left there. See [notes/what-emma-inherits.md] for what came
across, what did not, and why.

## Status

Concept and inventory. Nothing runs yet.

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
```

Emma also reads `.claude/` when it finds one, so configuration written for
Claude Code works without being rewritten. See
[notes/claude-code-compatibility.md].

## The model

The Anthropic API, directly. Emma owns its loop, so it needs a model it can
drive rather than a runtime that drives itself — the `claude` CLI and the
Agent SDK were investigated and dropped for that reason, recorded in
[notes/claude-code-compatibility.md].

Set the key once:

```
$ emma auth
```

It is stored under your home directory, readable only by you, and never in the
project you are working on.

[tustle-agent]: ../tustle-agent
[notes/what-emma-inherits.md]: notes/what-emma-inherits.md
[notes/claude-code-compatibility.md]: notes/claude-code-compatibility.md
