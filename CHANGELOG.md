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

- **Resuming no longer warns that your working directory changed when it did
  not.** The check compared the recorded path with the current one as raw text,
  so one directory spelled two ways — a trailing separator, forward slashes
  instead of back — was reported as a different project, with the alarming and
  untrue line that the write tools were pointed somewhere else. It compares them
  the way the rest of the resume path does now. A genuinely different directory
  still warns.

- **`config check` no longer tells you a working deny rule can never fire.** A
  rule like `Bash(sudo*)` was announced as "ends its prefix inside a word, so it
  can never match". It matches: the `*` is consumed as part of the prefix, and a
  rule matches the exact command it spells. The note now says that, and still
  points out the surprise — `sudo*` looks like a wildcard and is not — with the
  same remedy as before, `sudo *`.

  If you deleted such a rule because Emma called it dead, it was not.

- **New: `emma verify`.** Sends an independent reviewer at each outstanding row
  of the parity ledger — a fresh model with the read tools, briefed to disprove
  the row rather than confirm it — and writes a receipt with its verdict and its
  whole report.

  ```
  emma verify [--rows <ids>] [--limit <n>] [--dry-run] --model <id>
  ```

  It spends money: each row is a model run with tools. `--limit` defaults to 5
  and `--dry-run` reaches no model. Only a verdict of `UPHELD` closes a row.
  `--max-tokens` applies; without it a review gets four times the usual goal
  budget, because reviews read far more than goals do.

- **`--resume` now tells you when it passed over a session file it could not
  read.** Bare `--resume` means "the one I was last running here". If the newest
  file was corrupt it was skipped in silence and an *older* conversation was
  resumed instead — you got a resume, about the wrong work. It still skips,
  which is right; it now names the file and points at `emma --resume <id>`.

- **`emma agents` says when a session file could not be read.** Its output is
  all totals, and a session nobody could read contributed nothing to them while
  the numbers still looked complete.

- **Resuming a finished session and asking a plain question no longer reports a
  failure.** A conversational turn — one needing no tool and claiming no
  completion — was treated as a stall, nudged twice more, and ended as
  `stalled` or `kicks exhausted`. Under `-p` that is a non-zero exit code for a
  run that answered correctly. A goal that was genuinely cut off — by a budget,
  by the nudge count, by Ctrl-C — is still continued as before.

- **Emma now tells you when it stops recording a session.** If the transcript
  cannot be written — a full disk, a handle revoked underneath the run — the
  run continues, which is right, but you were not told. The file named in the
  exit line was quietly missing the end of the work, and `--resume` would not
  bring it back.

- **`emma config check` now lists frontmatter keys that do nothing.** If you
  wrote `allowed-tools:` in a skill or a command, Emma read past it and acted on
  nothing — and there was no way to find that out. It is dead in every file
  type; agents spell it `tools:`. A command's whole frontmatter block is
  discarded, so a `description:` meant for the menu never reached one, and the
  menu uses the file stem. `model:` works on `agents/*.md` and only when that
  agent is delegated to.

  Reported, not honoured: acting on `allowed-tools:` would mean pre-approving
  tools, which is not a decision to make on your behalf. And not a startup
  error, because `.claude` files are written for Claude Code and carry keys Emma
  has no business touching.

- **BREAKING: `WebFetch` now refuses a page that redirected to another host.**
  You approve a host — `example.com` — and Chrome then follows whatever
  redirect that host serves, including a JavaScript one. Until now the page it
  landed on was read back and handed to the model whatever host it came from,
  and a saved `WebFetch(domain:example.com)` rule meant that happened with no
  prompt at all. On a hostile page, the page chose the destination.

  The call now fails with a message naming both hosts, and nothing is read
  back. To follow the redirect, call `WebFetch` with the destination URL: you
  will be asked about that host, which is the point.

  A redirect that keeps the host — `http` to `https`, a path change, a trailing
  slash — is unaffected. A subdomain is a different host, so `example.com`
  redirecting to `www.example.com` now asks; approve it with
  `WebFetch(domain:*.example.com)` if you want that permanently. This is only
  as good as what Chrome reports: a navigation that failed has no destination
  to check, and this does not catch one it was not told about.

- **A `WebSearch` with no key explains itself again — and now there is a test
  saying so.** The message naming `BRAVE_SEARCH_API_KEY`, the credentials
  field, and where to get a key was never executed by any test, so it could
  have gone missing silently. Unchanged for users; listed because the guidance
  is the only thing a keyless user has.

- **`Glob` now says when a directory could not be opened.** `Grep` already did.
  A file list that quietly omits an unreadable subtree reads as complete, and
  "nothing matched" and "I could not look" were the same empty answer.

- **Pasting into the input box now tells you what it changed.** The box is one
  line, so a pasted block has its newlines turned into spaces, its tabs turned
  into single spaces, and any control bytes dropped. That was already true; it
  now says so, naming the counts, because the next thing that happens is a
  request billed against text that is not what you copied.

  The flattening itself is unchanged and needs a multi-line editor, which does
  not exist yet.

- **A browser profile Emma cannot delete is now reported.** When a goal ends,
  Emma kills the browsers it opened and removes their profile directories. On
  Windows those files can stay locked until the process actually exits, so the
  removal retries for about three seconds and then gives up. It used to give up
  silently. It now warns and names the directory, because that directory holds
  the session's cookies and may hold a login.

  Emma still clears it at its next start, as it always did.

- **`/copy` refuses more reliably.** It writes to the clipboard with an escape
  sequence, so it has always been refused when Emma is piped, run with `-p`,
  run under `EMMA_NO_FRAME`, or run with no console. That refusal now lives in
  the function that emits the bytes rather than only at the one place that calls
  it, and `/copy` reports what was actually sent rather than what it attempted.
  No change if you were not hitting the refusal.

- **A write that breaks a hard link now says so.** Emma writes files by
  building a complete copy and renaming it over the target, so a crash cannot
  leave your source half-written. That rename replaces the directory entry
  rather than writing through it, which means a file with two names on disk
  ends up with the edited name changed and **the other name still holding the
  old content**.

  Most editors behave the same way and a torn file is worse, so the write is
  unchanged. What is new is that `Write` and `Edit` count the file's names
  before writing and add a line to the result when there is more than one. The
  count is read first because after the rename it is gone.

- **Three tool pages open inside Emma instead of launching programs.**
  `Alt+,` is Settings, `Alt+d` is the Data Explorer, `Alt+m` is Memory, and
  `Esc` returns to the conversation. The sidebar, the status bar and the
  transcript are all where they were — a page replaces the middle of the frame
  and nothing else, and leaving one loses no scroll position.

  **`Alt+,` used to open your editor on `~/.emma/settings.json`, and `Alt+d`
  your file manager.** They no longer do. If you preferred that, say so — the
  external launch was deliberate and is easy to bring back.

  Each page shows only values that exist, with where they came from, and names
  what it is *not* drawing rather than showing a control that would not act.
  Settings names seven such panels; the Data Explorer says why there is no query
  box; Memory says plainly that Emma has no embedding index, no retrieval and
  therefore no similarity scores. An empty panel invites you to assume the
  number is somewhere else, so the pages say it in words.

  QUICK HELP's `Esc` row now reads `close menu / leave page`, and `Alt+key`
  reads `tool or page` — three of the seven chords no longer launch anything.

  `Alt+m` previously produced a warning claiming "the frame owns the 'm' key"
  about a key nothing had claimed. That is fixed in both directions: the page
  exists, and the frame routes the key.

- **`/copy` puts the last answer on the clipboard, and `/export` writes the
  conversation to a file.** Both take the text from the session log rather than
  from the screen, so you get the markdown the model wrote — not something
  wrapped to a column with a sidebar beside it, which is what a mouse selection
  of the same answer gives you.

  `/copy` uses OSC 52 and is **refused** on `-p`, on a pipe and with no console,
  because those runs must contain no escape byte at all; it tells you to use
  `/export`, which works everywhere. Emma cannot tell whether the terminal
  accepted a clipboard write — there is no acknowledgement in the protocol — so
  it reports what it sent and never that it arrived.

  If a session file has records that cannot be read, `/export` says so at the
  top of the file rather than producing a transcript that reads as complete.

- **Hooks accept Claude Code's `args` field.** A `settings.json` carrying it was
  previously refused as *malformed* — a file valid for the program it was
  written for, reported as your mistake. Arguments are passed as a real argv,
  never joined into a string and never through a shell.

  `shell: true` is now read and then **refused with a sentence** naming what to
  do instead, rather than being an unknown field. Emma execs a contained argv
  with a cleared environment and has no shell to offer; running such a command
  anyway would give it different semantics than it was written for.

  A hook Emma cannot honour now costs **that hook** rather than the whole
  session: under `EMMA_CLAUDE_HOOKS=skip-unknown` it is named and skipped, so a
  settings file with one unusable hook still boots.

- **`Bash` can run a command in the background.** Pass `run_in_background: true`
  and the call returns a task id immediately; `BashOutput` returns what has
  arrived since you last asked, and `KillShell` stops it. `timeout_ms` together
  with `run_in_background` is refused rather than ignored, and `KillShell`
  signals the shell it started — never a process tree — which the outcome says.

- **`Bash` can now run a command in the background, and two new tools read and
  stop it.** Pass `run_in_background: true` and the call returns immediately
  with a task id instead of waiting; `BashOutput` returns whatever that task has
  produced since the last read, along with whether it is still running; and
  `KillShell` stops it. The names match Claude Code's, so a hook matcher or an
  allow-list written for one works for the other.

  Three things worth knowing before you use it. A background result is a receipt
  for a *start*, not for a finish — nothing has been waited for, and the outcome
  says so, because a model that reads a spawn as a completed build will report
  success for work that has not happened. `timeout_ms` together with
  `run_in_background` is **refused** rather than ignored, since there is no wait
  to bound and accepting an argument that does nothing is worse than saying no.
  And `KillShell` signals the shell it started, never a process tree: a command
  that launched a server leaves that server running, which the outcome states
  rather than implying otherwise.

  Output is capped at 256 KiB per task, oldest first, and a read that lost bytes
  says how many and names the cap.

  If you write permission rules, note the surface grew: `Bash(...)` rules do not
  cover `BashOutput` or `KillShell`, which are separate tool names and need
  their own entries. `BashOutput` is read-only; `KillShell` is not.

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
  never launch anything; they type, as before. Which programs the launches use
  is configurable: a `tools` block in `~/.emma/settings.json` with `shell`,
  `editor`, `file_browser` and `data_dir` keys. Each value is one program name
  or an absolute path, never a command line — a value with arguments in it is
  refused rather than word-split. An absent key means Emma probes the machine,
  as it did before.
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
