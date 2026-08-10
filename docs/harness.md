# The harness

The harness is the product. Emma is what you get when you point it at a
configuration directory: the loop is fixed, and personas, skills, commands and
hooks are what make one Emma different from another. This document is about
`crates/harness` — the `.emma/` layout, how the directory is found, what
transfers from `.claude/` and what does not, and why a hook is treated as a
subprocess supervision problem rather than a config field.

See [architecture.md](architecture.md) for where this sits, [the-loop.md](the-loop.md)
for the two points at which hooks fire, and [approval.md](approval.md) for the
one thing a hook outranks.

## The shape of the crate

Everything here exists to turn a directory into one value. `Harness`
(`crates/harness/src/lib.rs`) is what the loop consumes, and by the time it
exists every question has been answered: `instructions` is an assembled `String`,
`persona` is a resolved `Option<String>`, and skills, commands and hooks are
private fields behind accessors. Nothing in `agent.rs` reads a file.

The consequence is that the boot states are not open. They are:

- **Absent** → refuse to start, naming every path searched.
- **Present but empty** → boot, and answer nothing. `is_empty()` is true, and no
  code path anywhere special-cases it. That it needs no code is the proof the
  harness is separable from the engine.
- **Malformed** → refuse to start, naming the file and the problem.
- **`.emma/` persona files that nothing selects** → refuse. An empty harness is a
  statement; an unselected one is an accident.

The reasoning is that an agent booted with the wrong prompt does not crash. It
acts fluently and confidently, attributed to an `instructions_hash` nobody
reviewed — and in Emma it acts by writing files and running commands. So any
ambiguity about _what the model was told_ resolves to not starting. There is no
compiled-in fallback prompt.

## `.emma/`

```text
.emma/
  config.json          the one file that is configuration rather than prompt text
  personas/
    _shared/
      rules.md         layer 1
      business.md      layer 2
    <persona>/
      rules.md         layer 3
      soul.md          layer 4
  skills/
    <name>/SKILL.md    YAML frontmatter (name, description) + markdown body
  commands/
    <name>.md          expanded at intake when the user types /<name>
  hooks/
    <anything>         executables; a hook command must canonicalise to in here
  tasks/tasks.md       written by the task tools, not read as configuration
  sessions/            JSONL transcripts (under ~/.emma/, not a project harness)
  credentials.json     written by `emma api` (under ~/.emma/)
```

`config.json` is `deny_unknown_fields` throughout — Emma's own format, so a key
Emma does not recognise can only be a typo. A misspelled `mathcer` that quietly
matched every tool is the failure that costs one attribute to prevent. The shape
is `default_persona`, a `personas` map, and a `hooks` map; a persona block carries
`description` (prose, nothing branches on it), `tools`, `skills` and `hooks`.

### Personas

A persona is a directory of prompt layers, assembled in a fixed order with absent
files skipped and **nothing trimmed, normalised or re-wrapped** (`assemble`). With
one file present, the assembled prompt is that file's bytes — which is what makes
`instructions_hash()` comparable across a refactor and worth logging on every
model call. A file that exists but is empty is skipped so it cannot contribute a
stray blank-line separator.

Two things outside the prompt _are_ adjusted, and the rule is stated narrowly for
that reason: a skill body is taken from after the frontmatter with leading
whitespace stripped (`split_skill`), and a command body is trimmed at both ends
(`load_commands`). Neither is always-on prompt text — a skill body arrives only
when the model loads it, and a command body is text a person typed a `/name` to
summon.

A persona file cannot remove a shared rule, because assembly is plain
concatenation and the shared text is still there for a reviewer to see.
Overriding `_shared` is a review on `_shared`. That is a review discipline, not
an enforcement mechanism, and the code says so.

Selection is a runtime decision and never the model's: `EMMA_PERSONA` overrides
`default_persona`, and a model choosing its own persona would be a self-modifying
prompt. `_shared` is not a persona and can never be selected.

A persona's `tools` array is a **real allowlist** — `select_tools` consumes the
registry and returns a filtered one. In the predecessor it validated names and
removed nothing, which was survivable when every tool was a read-only search and
is not survivable for a surface that includes `Bash` and `Write`. The startup
assertion is kept alongside the filter: a name the binary does not register is a
load error, because filtering without asserting turns `"Bahs"` into "no shell for
you" with no explanation.

### Skills

`skills/<name>/SKILL.md`, two frontmatter fields read and no more: `name` and
`description`. Anything the runtime must branch on belongs in the spine. A skill
resolves to a `SkillDef` with a `hash` over the body only, so it identifies the
exact text a turn was given independent of how the catalogue described it.

The catalogue is sorted by name, always, and a persona's `skills` array is
therefore a **set and not an ordering**: the catalogue rides in the prompt prefix
that the provider caches as an exact match, so neither directory iteration order
nor the order an operator happened to type must decide the prompt bytes. The same
rule governs `select_tools`, which iterates registry order rather than allowlist
order.

`skill_catalog()` returns `None` when there are no skills, and `main.rs` uses that
to skip registering the `Skill` tool entirely. Returning an empty string instead
would make the mistake invisible at the call site; offering a capability that
cannot work is a trap with a description attached.

The `Skill` tool itself lives in `crates/emma/src/skill.rs`, not in a tool crate,
because it is the one tool whose content comes from the harness. Its schema's
`name` is a **closed enum** of exactly the resolved skills, and `invoke` enforces
the enum again rather than trusting it. A tool that takes a path and reads it is
an arbitrary-file-read tool with a friendly description.

### Commands

`commands/<name>.md`. `Harness::expand_command` is server-side expansion at
intake: `main.rs` calls it on the raw line before a goal is constructed, so **the
model never learns commands exist**. An unknown `/word` returns `None` and passes
through as ordinary text. A tail after the command name is appended to the body
with a blank line between.

### Hooks

Declared in the `hooks` block of `config.json`: an `event`, a `command` relative
to the root, an optional `matcher`, an optional `timeout_ms`, and optional `text`.
A persona's `hooks` array selects which of them are live; `"hooks": []` turns all
of them off, and a hook the persona excludes is never resolved, so it costs
nothing.

`event` is a `String` in the declaration rather than the enum, so an unimplemented
event name produces Emma's sentence rather than serde's. Emma implements exactly
two — `PreToolUse` and `PostToolUse` — and a config naming any other Claude Code
event (`SessionStart`, `UserPromptSubmit`, `Stop`, …) is a **startup error, never
a silent skip**. A security hook that quietly never runs is worse than no hook,
because the operator believes they have one.

## Discovery

`discover()` walks up from the working directory the way git finds `.git`.
`EMMA_ROOT` overrides it outright, and an override that is not a directory is an
error rather than a fallback to the search — an override that fell back would
boot the agent on configuration nobody asked for.

Precedence is decided before anything is read. Nearest ancestor wins, and within
a single directory `.emma/` beats `.claude/` outright. The loser is ignored
entirely, **never merged**. Merging is convenient and is exactly how a
configuration system becomes impossible to reason about, because the answer to
"where did this instruction come from" stops being a file.

The walk stops at the home directory. Above it are `C:\Users` and `/home` —
directories that belong to the machine rather than to any project — so a harness
adopted from one of them is the same wrong-prompt boot with a longer walk.
Stopping there also keeps discovery's answer a function of the tree the caller
named.

Two rules apply _at_ the home directory, and both exist because the obvious
behaviour was wrong in practice.

**`~/.claude/` is never adopted as a project harness.** It is Claude Code's
user-scope configuration — global permissions, global agents, global skills, for a
different program. Adopting it would hand Emma standing instructions from a
directory the user never associated with this project. It is skipped so
completely that it never even reaches the `searched` list, because an error
listing a path Emma declined to consider reads as a bug in the search.

That skip has a subtlety worth knowing about: whether the walk is _at_ the home
directory is decided by canonicalising both sides and comparing paths, not by
comparing bytes (`real`, and the comment at `at_home` in `discover_in`). A
byte-comparison left the skip inert on a box where `$USERPROFILE` said
`C:\Users\Owner` and the walked path said `C:\Users\owner`, and the user's global
`.claude/` was adopted. Case is only one of the ways two spellings of one
directory differ — a trailing separator, a `..` in the middle, a Windows 8.3
short name and a home reached through a symlink are the others — so the question
is asked of the filesystem. `real` falls back to the path as given when it cannot
ask, which makes the comparison exact rather than wrong.

**A `~/.emma/` holding only credentials is not a harness.** It used to be eligible
for existing, and `emma api` creates it: storing a key wrote
`~/.emma/credentials.json`, and from that moment every project without a harness
of its own adopted the home directory and booted with no instructions, no persona
and the full write-capable tool surface. Nobody chose that; it was the side effect
of saving a key. So `configured_by_hand` requires `config.json` or `personas/` —
the two things that only exist because someone wrote them. Credentials,
`sessions/` and `settings.json` are Emma's own bookkeeping and a personal model
preference, and say nothing about how Emma is configured.

Note what that is _not_: it is not the empty-harness boot. Such a directory is not
a harness at all, the walk goes past it, and the operator gets the absent-harness
refusal — which names the directory anyway, with the reason, because a refusal
that omitted a directory the operator can see reads as a search that never
looked.

`discover_in` threads both the `EMMA_ROOT` override and the home directory as
parameters rather than reading the environment, and `Harness::load_selecting`
does the same for `EMMA_PERSONA`. That is not tidiness: environment mutation in a
test binary cargo runs threaded is a race, and a race in a test is a green light
nobody earned. The un-threaded version is how the `~/.emma/` defect above was
found — tests either mutated `HOME` or walked out of a scratch directory into the
developer's real home and answered differently on different machines.

## `.claude/` compatibility, honestly

Emma runs on the Anthropic API. It does not shell out to the `claude` binary and
does not use the Agent SDK (`notes/claude-code-compatibility.md`). What
compatibility means here is narrower and useful: recognise `.claude/`
configuration when we find it, so a skill or command already written for Claude
Code works unchanged.

### What transfers

| Claude Code                                 | How Emma reads it                                                                                 |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| `.claude/skills/<name>/SKILL.md`            | identical shape, same loader (`load_skills`)                                                      |
| `.claude/commands/<name>.md`                | identical shape, same loader (`load_commands`)                                                    |
| project `CLAUDE.md` and `.claude/CLAUDE.md` | prompt layers, in that order                                                                      |
| `.claude/agents/<name>.md`                  | the nearest thing to a persona: body is a prompt layer, frontmatter `tools` becomes the allowlist |
| `.claude/settings.json` hooks               | flattened into Emma's `name → HookDef` map                                                        |

Tool names match Claude Code's exactly, so a matcher or allow-list written for one
applies to the other.

Two deliberate relaxations, both arguing the same way. `.claude/settings.json` is
**not** `deny_unknown_fields` at its outer level: it is a foreign format that
legitimately carries `permissions`, `model`, `env`, `statusLine` and more that
Emma has no opinion about, and denying those would mean refusing to start in
essentially every real Claude Code repository. A rule that fires on correct
configuration is an outage, not a safety property. The **hooks block inside it is
strict**, because that is where a silently-ignored key costs something — a
misspelled `mathcer` there produces a hook that guards every tool instead of one
and looks like it is working.

The same argument one level down governs skill frontmatter. Under `.emma/` a
`SKILL.md` is strict: both fields required, `deny_unknown_fields` on, and a
failure takes the boot with it, because an unknown key can only be the operator's
typo in Emma's own format. Under `.claude/` it is permissive — real skills on a
working machine carry `model-role`, `version` and `allowed-tools`, and under the
strict struct every one of them took the whole boot down. Unknown keys are
ignored, `name` and `description` stay required (they are the catalogue line the
model chooses from), and a file that cannot be parsed at all is **skipped with a
warning on stderr rather than dropped silently** — a catalogue quietly one shorter
than the directory is a gap nobody notices until the model cannot find a skill
that is plainly there. `after_licence_header` additionally steps over HTML
comments and blank lines before the frontmatter, and nothing else; a file with
real prose above its `---` is not a skill with a header.

One more divergence, and it is the place a ported rule was wrong for Emma rather
than merely different: the "persona files nothing selects → refuse" rule applies
to `.emma/` only. In `.emma/` personas _are_ the prompt, so an unselected one is
an accident. In `.claude/` the prompt is `CLAUDE.md` and `agents/` holds optional
sub-agent definitions — a normal repository has a dozen and selects none. Applying
the refusal there would mean Emma declines to start in almost every Claude Code
checkout, which is not a safety property. Selecting an agent that does not exist
is still a refusal; that one really is a mistake.

### What does not transfer

**Hook commands.** This is the one place the two systems genuinely disagree, and
Emma does not blink. Claude Code's `command` is a shell string: it may pipe,
expand variables, or name any executable on the box. Emma canonicalises a path,
containment-checks it inside `<root>/hooks/`, and execs an **argv** with a cleared
environment. Honouring a shell string would mean dropping every one of those, at
the exact point where Emma is deciding whether to let a model run `Bash`.

So `translate_command` (`crates/harness/src/claude.rs`) accepts
`$CLAUDE_PROJECT_DIR/.claude/hooks/x.sh`, `${CLAUDE_PROJECT_DIR}/...`,
`.claude/hooks/x.sh` and `hooks/x.sh`, and refuses everything else with a startup
error that says what to do about it. A guard that cannot be honoured must not be
quietly dropped — the same ruling as the unimplemented event, for the same reason.

The refusal fires on any shell metacharacter (`SHELL_METACHARACTERS`) **or any
whitespace**. Whitespace is handled as a class rather than as an explicit `' '`
check, and that is a fix rather than a style: the explicit check left the tab out,
so `hooks/guard.sh\t--strict` walked past the refusal and failed further down as a
missing file — the right verdict with the wrong sentence pointing at the wrong
problem. Absolute paths are rejected separately, and the rootedness test is
hand-written because `is_absolute()` alone is not enough: on Windows
`/usr/bin/true` is a _relative_ path, so a unix-authored config would have been
refused for the wrong reason.

**Events other than the two.** Loud error, covered above.

**Hook types other than `command`.** `entry.kind != "command"` is a startup error.

**MCP servers.** Out of scope.

Hook names have no source in Claude Code's format, so they are synthesised from
position as `<Event>[g][h]`. Stable for a given file and deterministically
sorted, which is all the ordering contract needs. Claude Code counts `timeout` in
seconds; Emma's spine counts milliseconds, and the translation is explicit.

## Hook security

A hook runs an operator-authored program at the highest-privilege point of the
turn. `crates/harness/src/hooks.rs` treats that as a supervision problem, and the
split from `lib.rs` is not bookkeeping — the two files fail for different reasons.
`lib.rs` growing means configuration is sprouting behaviour; `hooks.rs` growing
means the supervisor is doing more to a hook than run it and read its answer.

Everything that can be checked before a hook ever runs is checked in `resolve`,
at load, so a hook that cannot be run safely stops the boot rather than failing at
the moment it was needed.

**Containment.** `<root>/hooks/` is canonicalised, the command is canonicalised,
and `command.starts_with(dir)` must hold. This is why `command` is relative: an
absolute path or a `..` escape would let the spine run any executable on the box
with Emma's permissions — and Emma's permissions include writing the user's source
tree.

**The executable check.** Unix only, because there is no executable bit on Windows
to consult; a `.cmd` or `.exe` is runnable by extension. It is explicitly _not_
the containment boundary — `starts_with` is, and it applies everywhere. What it
catches is the operator who wrote a hook and forgot to `chmod +x` it, which would
otherwise surface as a spawn failure at the first tool call and, `PreToolUse`
being fail-closed, as a denial.

**`env_clear`.** The hook gets six names and no more:
`PATH`, `HOME`, `LANG`, `TMPDIR`, `SYSTEMROOT`, `COMSPEC` (`HOOK_ENV_ALLOWLIST`).
The last two are Windows process-creation requirements, not policy. This process
holds `ANTHROPIC_API_KEY`; a hook is operator-authored but is still a separate
program, and it gets what it needs to execute and nothing that would let it call
a model or a paid API as us. A hook that needs a value reads it from a file next
to itself.

**Caps.** Timeout defaults to 5s and is capped at 10s — config asking for more is
silently reduced rather than refused, because the operator asked for a longer
gate, not a different program. Each pipe is capped at 64 KiB, and a hook that
writes past the cap blocks and hits the timeout, which is the right answer for a
program that will not stop. A reason string is capped at 400 characters. The
executable's contents are hashed at resolve time and the hash appears in
`snapshot()` and in every `HookRun` record, so what ran is identifiable.

**The matcher is anchored.** `^(?:{m})$`, so `Read` cannot silently guard
`ReadFile` — the near miss that looks like a working policy.

**No shell, no PATH lookup, no argument string.** `Command::new(&self.command)`
with a canonicalised path, `kill_on_drop(true)`, and the payload delivered on
stdin as JSON. There is no argument string for a tool name to be interpolated
into.

**What the hook is told, and what it may say back.** `HookCall` carries the tool
name, the call id, the args, the session and turn ids, and — on `PostToolUse` only
— a `HookResult` with the model-visible `content`, `truncated`, and the error kind
if the tool failed. It is deliberately not built from a `&ToolOutcome`: `display`
is for the terminal and is not what the model saw, and a hook shown more than the
model is shown would be a side channel the operator did not know they were
operating. Widening it requires a signature change here.

Back the other way, a hook answers with JSON on stdout: `decision`, `reason`,
`context`, `deny_unknown_fields`. Empty stdout with exit 0 is the normal answer
for an observer and means allow. `context` is **additive** — a hook can annotate a
model-visible result, and there is no field anywhere that rewrites one.

### The fail-closed / fail-open asymmetry

Four lines in `hooks::run`, and they are the whole hook design:

```text
denied = run.outcome != HookOutcome::Allow      // deny, timeout, crash,
                                                //   non-zero exit, garbage stdout
if denied && event == HookEvent::PreToolUse:
    verdict.denied = Some(reason); return
```

A `PreToolUse` hook that denies — or times out, crashes, exits non-zero, or
answers with garbage — **stops the call**. Ambiguity resolves to deny, because a
broken policy check must not degrade into no policy check. `HookRun::outcome` is
initialised to `Failed` and every early return in `invoke` leaves it there, so the
default is the one that denies.

A `PostToolUse` failure **changes nothing**. The tool ran, the side effect
happened, the result is already recorded, and pretending otherwise would make the
log lie about what the model saw.

Nothing in `hooks.rs` writes to a log or knows an event type; the caller owns any
record it wants to keep, which is what lets every test in the crate run without a
process around it. The loop's side of this — where the `PreToolUse` check sits
relative to the human — is in [the-loop.md](the-loop.md) and
[approval.md](approval.md).

## Identity, and what `config check` prints

`Harness::snapshot()` carries identity only, never prompt text: root, flavor,
persona, `instructions_hash`, `config_hash`, whether the harness is empty, the
tool allowlist, skill names with their body hashes, command names, and one line
per hook giving its name, event and command hash. A snapshot carrying prompt text
would be a second copy to drift.

`emma config check` prints that plus the resolved tool list, the registry's
`schema_hash`, any web tools this machine could not provide, the resolved model
and where it came from, and whether an API key resolves — reported, never printed.
It calls no model, which is the point: everything that can fail at startup fails
there, where the message is the only output rather than a preamble to one.
