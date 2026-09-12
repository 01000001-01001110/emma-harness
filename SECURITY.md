# Security

Emma is a coding agent: it reads your files, writes files, runs shell commands
and talks to a model provider over the network. This file says what it promises,
what it does not, and how to report something that breaks a promise.

## What Emma does not promise

**The approval gate is a consent interface, not a sandbox.** Before Emma writes
a file or runs a command it shows you what the tool declared it would do and
asks. It does not confine the tool. `Bash` can do anything your shell can do,
including undo the gate, and a command you approve runs with your account's full
authority. Treat approving a command exactly as you would treat typing it
yourself.

So the following are the tool working as designed, and are not vulnerabilities:

- A model asked to run a destructive command, you approved it, and it ran.
- A tool did something outside the working directory. Emma does not confine
  paths to the project.
- `--dangerously-skip-permissions` skipped permissions. It announces itself on
  every run and cannot be set from a configuration file.
- Instructions inside a file Emma read influenced what it did next. Everything
  Emma reads reaches the model. Do not point it at a repository you would not
  read yourself.

**One person, one machine.** There is no server, no listening socket, no
accounts and no multi-user model. Emma has no notion of an untrusted local user:
anyone who can run commands as you can already do everything Emma can.

## What Emma does promise

These are the properties a report can be filed against. Each one is a defect if
it turns out to be false.

- **Approval cannot be granted by configuration.** Answering `a` allows one tool
  for the rest of that process and no longer. No permission outlives a run, and
  none can be pre-granted from a file.
- **`-p` denies rather than assumes.** With nobody to ask, anything needing
  approval is refused and the model is told why.
- **Credentials do not reach the terminal, a log or a transcript.** A key
  registered through `ApiKey` is scrubbed from error formatting. A key appearing
  anywhere Emma prints is a defect.
- **`~/.emma/credentials.json` is created `0600` on unix**, by opening it with
  that mode rather than chmodding after — between those two calls the key would
  be world-readable on disk. Reading it back refuses rather than warns if the
  mode has since been loosened, because a warning about a key other accounts can
  read is one nobody acts on until after the key is gone. Windows has no mode
  bits; the file lands in the user's profile, whose default ACL already excludes
  other users, and Emma does not tighten it further.
- **A new host is announced before Emma talks to it.** If `OLLAMA_HOST` points
  somewhere other than this machine, Emma names the destination before the first
  call, because a stale value in a shell profile would otherwise ship your
  conversation and file contents to a box you had forgotten about.
- **Redirected output contains no escape bytes**, on every path including `-p`,
  and that includes escape bytes a tool emitted into its own stdout.

## Reporting a vulnerability

**Do not open a public issue for a security report.**

Use GitHub's private vulnerability reporting on
<https://github.com/01000001-01001110/emma-harness> — the **Report a vulnerability**
button under the Security tab. It opens a private advisory visible only to the
maintainer.

A useful report says which version or commit you tested, your OS, which of the
promises above it breaks, and the shortest sequence that reproduces it. If you
are not sure whether something is a vulnerability or the consent model working
as intended, report it privately and say you are unsure.

## What to expect

Emma is maintained by one person and there is no response-time commitment. You
will get an acknowledgement and a decision on whether it is a defect. A fix
lands on `main` with a `CHANGELOG.md` entry describing what changed; there is no
backport branch. A fix is released by tagging a new version from `main`, not by
patching an old one.

Only the current `main` and the most recent tagged release are supported. Emma
is `0.x`; its first binaries, `v0.1.0` for Windows x86_64 and macOS aarch64,
were published on 2026-09-09. They are unsigned: macOS will refuse to open the
binary until it is cleared with `xattr -d com.apple.quarantine emma`, and
Windows SmartScreen will warn. A checksum or provenance attestation is not yet
published, so a downloaded binary should be treated as trusted only as far as
the GitHub release page that served it.
