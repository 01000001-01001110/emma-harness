# Getting started

From nothing to a finished goal. Roughly ten minutes, most of it waiting for
`cargo`.

If you already use Claude Code in the repository you care about, skip to
[Already using Claude Code](#already-using-claude-code) — you may not need
`init` at all.

## 1. Install

Take a binary from a [tagged release], or build it:

```
$ git clone https://github.com/01000001-01001110/emma-harness
$ cd emma
$ cargo build --release
```

The binary lands at `target/release/emma` (`emma.exe` on Windows). Put it on
your `PATH`, or call it by path — nothing below depends on which.

Emma runs anywhere Rust does. Two things are looked up at run time rather than
required at build time, and both degrade rather than fail: a POSIX-ish shell
for `Bash` (on Windows the one Git for Windows installs is found
automatically), and Chrome for `WebFetch`. Without Chrome, `WebFetch` is simply
not offered.

[tagged release]: https://github.com/01000001-01001110/emma-harness/releases

## 2. The key

```
$ emma api
Anthropic API key (not echoed):
```

It is stored under your home directory, never in the project you are working
in — a test exists specifically to prove that a `credentials.json` sitting in a
repository is ignored, because Emma works inside repositories and reading one
would be a way to steal a key by checking one in.

For scripts, the key can be an argument or piped:

```
$ printf %s "$KEY" | emma api
```

**One thing that catches people.** If `ANTHROPIC_API_KEY` is set in your
shell, it wins over the stored file. `emma api` says so when it notices, and
`emma config check` tells you which one is in use. If you store a key and Emma
still complains, that variable is usually why.

Emma is Anthropic-only today. There is no provider setting because there is
nothing to choose between; if that reads as a limitation, it is one.

## 3. The model

```
$ emma model                      # what am I using?
$ emma model claude-sonnet-5      # use this from now on
$ emma --model claude-sonnet-5 "…"  # just this once
```

The built-in default is `claude-opus-5`. `emma model <name>` writes your
choice to `~/.emma/settings.json`; `--model` overrides it for one run and
changes nothing on disk. Different words, different lifetimes, deliberately.

Emma does not validate the name. A model id it has never heard of is passed
through, and a 404 from the API is information rather than a plumbing failure.

## 4. `emma init`

Emma will not start in a directory with no configuration. That is deliberate —
a silent fallback would mean running with a prompt nobody chose — but it means
a fresh checkout needs one command:

```
$ cd ~/code/my-project
$ emma init
created ~/code/my-project/.emma/config.json
created ~/code/my-project/.emma/personas/assistant/rules.md

Edit …/rules.md to say what this agent should know and how it should work.

Next:
  emma config check      load it and print exactly what the model will be told
  emma "<goal>"          state a goal
```

Two files, both real. `config.json` names one persona and selects it;
`rules.md` is the prompt. There is no wizard and nothing commented out — a
template you have to finish before it works is the same dead end one step
later.

**`init` refuses if `.emma/` already exists.** It will not merge, overwrite or
repair. Configuration that merges is configuration nobody can reason about; if
you want a fresh one, move the old one aside.

The file to edit is `rules.md`. It is the whole prompt, and the difference
between a generic assistant and one that knows your project is usually a
paragraph.

## 5. Check before you spend anything

```
$ emma config check
```

Loads everything and prints what the model will be told — persona, instruction
digest, tools, skills, commands, hooks, model, and whether a key resolved. **It
calls no model and costs nothing.** When Emma does something you did not
expect, this is the first thing to run, because most surprises are a
configuration you did not know you had.

It also reports capabilities that are _missing_ and why:

```
tools    Read, Write, Edit, Glob, Grep, Bash, TaskCreate, …, WebFetch
         WebSearch is not available: no Brave Search key. Set
         BRAVE_SEARCH_API_KEY, or add "brave_search_api_key" to
         ~/.emma/credentials.json
```

A tool that cannot work is not offered. Offering one is how a model spends a
turn discovering it was lied to.

## 6. A goal

```
$ emma
> read src/auth.rs and tell me what happens when the token has expired
```

or in one line, or without prompts at all:

```
$ emma "make the tests pass"
$ emma -p "list every TODO in src/ with its file and line"
```

Read, Glob and Grep run silently. Write, Edit and Bash will ask, and the prompt
shows the command, the diff, or the path and size — enough to judge, because a
prompt you cannot evaluate is one you learn to approve without reading.
Answering `a` allows that tool for the rest of the process and no longer.

`-p` has nobody to ask, so anything needing approval is denied and the model is
told why. That makes it safe in a script and useless for a goal that must
write; for those, `-p --yes` turns the gate off and says so.

The full reasoning is in [approval.md](approval.md).

## 7. Picking up where you left off

```
$ emma --resume "now do the same for the refresh path"
$ emma --resume sess-1786359652847-35228 "…"
```

Session ids look like `sess-<milliseconds>-<pid>` and are the filenames under
`~/.emma/sessions/`. There is no command that lists them yet; `ls` is the
answer today.

Bare `--resume` continues the newest session **started in this directory** —
not the newest overall, which would routinely be a different repository.

It brings back the conversation and the budget that was already spent. **It
re-runs nothing**: the model gets the record and decides what to redo, because
re-running a read is free and re-running a write is not, and the loop cannot
tell which it is about to do. You still type a goal; the restored conversation
is context, not an instruction.

If the harness or the model changed since that session, Emma says so and
continues.

## Already using Claude Code

Emma reads `.claude/` directly. A repository set up for Claude Code runs
without being rewritten, and you can skip `init`.

**`.emma/` wins outright when both exist.** Nothing is merged — `.claude/` is
used only in its absence. `emma config check` prints which one was loaded.

What Emma takes from `.claude/`:

|                       |                                                                                      |
| --------------------- | ------------------------------------------------------------------------------------ |
| the prompt            | your project's `CLAUDE.md`, then `.claude/CLAUDE.md`, then the selected agent's body |
| personas              | `.claude/agents/*.md`                                                                |
| skills                | `.claude/skills/`                                                                    |
| commands              | `.claude/commands/`                                                                  |
| hooks, tool allowlist | `.claude/settings.json`                                                              |

### What is different, and will eventually matter

**Hook commands are shell strings in Claude Code. Emma execs an argv and never
runs a shell.** Several known-good relative forms resolve; anything that needs
a pipe, a redirect or expansion is refused _at startup_, with a message saying
what to do. It is refused loudly rather than quietly not run, because a
security hook that silently never fires is worse than no hook — and renaming a
script is cheaper than discovering this at run time. This is the difference a
real user actually hits.

**Unknown keys are tolerated.** `.claude/settings.json` carries keys Emma has
no opinion about, and skill frontmatter routinely carries `model-role`,
`version` or `allowed-tools`. Those are ignored, and a skill file Emma cannot
parse is skipped with a warning rather than taking the boot down with it. A
rule that fires on correct configuration is an outage, not a safety property.

`.emma/` is strict in exactly the place `.claude/` is not: there, an unknown
key is your own typo in Emma's own format, so it is an error.

**An agent nothing selects is fine.** In `.emma/` a persona _is_ the prompt, so
one nothing selects is an accident and Emma refuses to start. In `.claude/` the
prompt is `CLAUDE.md` and real repositories routinely have agents selecting
none.

The honest summary: a repository already set up for Claude Code runs. It is not
a claim that every Claude Code feature is implemented, and hooks needing a
shell are the one you will meet.

## When something goes wrong

**"no `.emma/` or `.claude/` found"** — Emma lists every directory it searched.
Run `emma init` here. If you expected an existing harness to be found, the list
tells you what it looked at and in what order.

**A `~/.emma` that holds only a key is not a harness.** Storing credentials
creates that directory, and adopting it as configuration would mean every
project without its own setup silently running on an empty prompt. It is only a
harness if it holds a `config.json` or a `personas/` — something a person put
there on purpose. The refusal names it and says so.

**"the API rejected this key (HTTP 401)"** — the key is wrong, or
`ANTHROPIC_API_KEY` in your shell is shadowing the good one. `emma config
check` says which is in use.

**It stopped and said which limit it hit.** Iterations, tokens, wall clock, or
nudges. Every ending names its limit and the flag that raises it; nothing stops
silently.

**It stopped without finishing.** Emma stops when the model claims the goal is
met. Done-detection is the model's own claim today — the guarantee is that the
loop will not stop _before_ it, not that the work is finished. Read what it
did, and say what is still wrong.

## Where to go next

- [architecture.md](architecture.md) — the crates and the boundaries that matter
- [the-loop.md](the-loop.md) — one goal end to end
- [harness.md](harness.md) — everything `.emma/` can hold
- [approval.md](approval.md) — the gate, in detail
