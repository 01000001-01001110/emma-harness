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

- Prompt caching now uses each model's own minimum cacheable prefix instead of
  assuming Opus 5's 512 tokens. On models with a higher floor —
  `claude-haiku-4-5` and `claude-opus-4-6` need 4,096 — Emma was marking
  prefixes the API accepts but never caches, so those sessions paid full price
  for the marked span, with no error and nothing on screen to show it. If you
  ran `emma model claude-haiku-4-5`, switched with `/model`, or delegated to a
  Haiku-typed agent, this was costing you money. Models Emma does not know are
  gated at the highest known floor, which forgoes some caching rather than
  silently wasting it.
- A hook or `statusLine` program that starts something in the background and
  then exits is no longer reported as having timed out. Emma waited for the
  program's output pipe to close, and a background process it started held that
  pipe open — so a script that answered in milliseconds was recorded as
  `timed out after 5000ms` and its output thrown away, which for a status line
  meant on every repaint. Emma now waits for the program itself. Anything the
  program started is deliberately left running: Emma supervises the program it
  ran and makes no claim about that program's children, so a hook that hangs
  past its timeout is killed alone. Point a background process's output
  somewhere other than the hook's stdout — Emma stops reading it, and on unix a
  write into the closed pipe will kill an unprepared process.
- A `domain:` permission rule that can never match a real host is now reported
  at startup, the way `Bash(rm *)` already was: a non-ASCII domain
  (`WebFetch(domain:bücher.example)`) or an unbracketed IPv6 address
  (`WebFetch(domain:::1)`). Hosts arrive already punycode-encoded and bracketed,
  so those rules match nothing — in a `deny` list, a protection that protects
  nothing. The message says so and gives the spelling that works
  (`xn--bcher-kva.example`, `[::1]`). **No rule's matching behaviour changed**:
  anything that matched before still matches.
- The delegation footer now accounts for every tool call a subagent made.
  `WebFetch` gets its own `fetched (N):` line, `WebSearch` joins the existing
  `searched for (N):` line, and anything else is listed as
  `other tool calls: Skill ×1, TaskCreate ×3`. Before this, a call whose
  arguments were not `file_path`, `pattern` or `command` was counted in the
  total and named nowhere — five page fetches read as `files read: none` over
  `5 tool calls`.
- The footer's `files read (N):` line is now `files touched (N):`. `Write` and
  `Edit` always landed in that list too, so "read" over-claimed. The
  `files_read` field in the `delegation` session-log record keeps its name; two
  new fields, `fetched` and `other_tool_calls`, sit beside it.
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
