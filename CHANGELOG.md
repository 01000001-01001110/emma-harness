# Changelog

What changed, for someone deciding whether to upgrade. The reasoning behind a
change lives in its commit message; what was learned lives in `notes/lessons/`;
where the work stands lives in `notes/STATUS.md`. This file answers only: what
is different now, and will it break me.

## How versions work here

Emma is `0.x`, and it is many months from `1.0.0` if it ever gets there. Until
then the rule is the pre-1.0 reading of semantic versioning, which is narrower
than people usually assume:

- **`0.MINOR.0` — a breaking change, or a new capability.** Anything that
  changes a command, a config file's shape, a tool's arguments, or what the
  terminal does. Before `1.0.0` there is no promise of compatibility, so a
  breaking change is a minor bump rather than a major one.
- **`0.MINOR.PATCH` — a fix.** Behaviour that was already meant to work, now
  working. No new surface, nothing to migrate.

The version is in the workspace `Cargo.toml` and applies to the whole
repository; the crates are not published separately.

**Breaking changes get a `BREAKING` line saying what to do about it.** A
changelog that says "improved configuration handling" over a config file that no
longer loads is worse than silence, because it costs the reader the time to find
out for themselves.

## Unreleased

- Closing a browser session no longer leaves the session's Chrome profile
  behind in your temp directory. It used to, on Windows, about one close in
  eight — a whole profile, cookie database included, sitting in a shared folder
  until something else swept it. Closing now waits for the browser to actually
  exit before removing the directory.
- Opening two browser sessions at the same time no longer fails one of them with
  `The system cannot find the path specified. (os error 3)`. Both sessions share
  the `.browser-miner` directory, and one closing could delete it while the
  other was still writing its record. Failures that do get through now name the
  operation and the path instead of a bare io message.
- `browser-miner session close` says whether the profile directory really went:
  each entry gains `profile_removed`, and a `profile_dir` naming the path when
  it did not. It reported success unconditionally before, which is how the leak
  stayed invisible.
- The full-screen sidebar collapse now works by the routes a person actually
  tries: clicking the `[+]` on the SESSIONS header collapses the sidebar, and
  `Ctrl-B` still toggles it from the keyboard (listed in QUICK HELP and the
  hint row). A collapse or expand you chose sticks; resizing the window no
  longer overrides it. Shift+drag selection is untouched — only a plain left
  click is routed.
- The sidebar's TOOLS section lists the user tools (Shell, Code, File Browser,
  Search, Memory, Data Explorer, Settings) with their real bindings:
  `Alt+<key>` launches — `Alt+s` a shell, `Alt+,` settings, and so on — and
  `/` is Search's surface, the command menu it already opens. A tool that
  cannot launch on this machine shows `n/a` in the key column, and every
  launch reports what happened, or why not, in the transcript. Bare letters
  never launch anything; they type, as before.
- The full-screen layout now matches the mockup image where it previously did
  not: two columns of ground between the sidebar and the main pane, one blank
  row between the main pane and the status bar, panes floated one cell inside
  the window edge on roomy windows, and the input box carries the mockup's
  `[send: Enter]` hint inside its right edge.
- The sidebar itself is redrawn to the image's measured grid: the current
  session is accent text on a subtle full-width band (not a solid pink bar),
  the `[+]` is accent and sits on the same right edge as the dates and keys,
  headers and rows are indented as the mockup has them, an unavailable tool
  dims as a whole row, and the QUICK HELP table is uniformly dim.
- **New: `/theme` in a running session.** `/theme` lists the themes this
  machine and this project have, marks the one that is selected, and — on a
  fresh machine, which has none — says so and gives both directories a theme
  file can go in. `/theme <name>` selects it and writes the name to
  `~/.emma/settings.json`, leaving every other key in that file alone. **The
  colours change at the next start, not immediately**, because a theme is read
  once when Emma starts; `/theme` says which theme the screen is still showing.
  A name that is not there, or a file that will not parse, is refused with the
  list of what can be chosen, and nothing is written. There is no `--save` —
  selecting is saving — and `/theme <name> --save` says so rather than being
  quietly accepted. On a run with no colour at all (`NO_COLOR`,
  `EMMA_COLORS=none`, or piped output) the selection is still stored and
  `/theme` says plainly that restarting will not look any different.
- A theme selected in `~/.emma/settings.json` is now read at startup and used
  for the whole run. A theme file that is missing, that will not parse, or that
  has a bad value in one place costs colour and never the boot: Emma falls back
  as far as it has to, and says so in words before the first prompt.

## 0.1.0

The first version worth naming. Everything below already existed before this
file did; it is recorded here so the next entry has something to be a change
_from_.

### The loop

- A goal-holding agent loop with failure-as-observation: a tool that fails comes
  back as a result the model can read and act on, never as an abort.
- A "kick" that nudges the model once when it stops without claiming completion,
  bounded by an iteration budget and a stall rule.
- Compaction that replaces whole goals oldest-first when the conversation
  approaches the context limit, deterministically and without a model call.
- Continuous conversation across goals, with an append-only JSONL session log
  and `--resume`.

### Tools

- Filesystem: `Read`, `Write`, `Edit`, `Glob`, `Grep`, `Bash`. `Edit` takes
  either a literal string or a `LINE#HASH` address, and refuses rather than
  applying to a line that changed underneath it.
- Tasks, for a goal-holding loop to write down what is left.
- Web: `WebSearch`, and a `WebFetch` that renders in real Chrome. Five browser
  tools for driving a page — open, read, act, fill, close. Nothing submits a
  form, deliberately.
- Code intelligence over LSP: `FindReferences`, `GoToDefinition`, `Hover`,
  `DocumentSymbols`.
- Every tool that truncates says which limit cut it, how much was lost, and how
  to get the rest — or says plainly that no argument raises it.

### Delegation

- `Delegate` runs a subagent with its own registry, budget and instructions,
  serialised so two subagents never race for one keyboard.
- Agent types are `.claude/agents/*.md`, unchanged from Claude Code's format.
- `emma agents` reports what ran, with no key, model or harness needed.

### Consent

- An approval gate with two axes: whether a tool can damage the machine, and
  whether it reaches the network. Network grants are per host.
- Permission rules in Claude Code's format, remembered in `settings.local.json`.
  `deny` outranks `allow`, and outranks `--dangerously-skip-permissions` —
  a flag that turns off prompting does not turn off policy.
- `PreToolUse`, `PostToolUse` and `UserPromptSubmit` hooks, the last of which
  can inject context into a turn or block it outright.

### Configuration

- `.emma/` and `.claude/` are both read: CLAUDE.md, agents, skills, commands,
  settings.
- Provider-scoped credentials and per-provider model selection.
- `max_tokens` and reasoning effort follow the model rather than a constant.
- In-session `/model`, `/compact`, `/clear`, `/config`, `/agents`, `/help`,
  `/exit`.

### Terminal

- A full-screen interface: a sidebar, a two-column conversation pane, an input
  dock and a status bar.
- Markdown rendering, diffs shown before a write is approved, and a configurable
  status line.
- **BREAKING**: Emma now uses the alternate screen. The transcript no longer
  lands in the terminal's own scrollback, native selection is reduced, and the
  terminal's search does not see the conversation. Set `EMMA_UI=inline` for the
  previous behaviour; that escape hatch is temporary and will be removed.
- Piped and `-p` output is unchanged and contains no escape sequences.
