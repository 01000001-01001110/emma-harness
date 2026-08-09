Run a shell command.

The command is passed to `sh -c`, so pipes, redirection, `&&` and shell globbing
all work. It runs in the working directory (or in `cwd`, which must be a
subdirectory of it).

- `timeout_ms` defaults to 120000 and is capped at 600000. On timeout the
  process is killed and the call fails, reporting whatever output was produced
  first.
- Combined output is capped at 64 KiB per stream; the result says so when
  output was cut.
- The environment is reduced to a fixed allowlist. It carries no API keys.
- A non-zero exit status is a failure, and the exit code and output are
  reported. Commands that use exit status to answer a question — `grep -q`,
  `test`, `diff` — will therefore read as failures; use `Grep` for searching,
  or append `|| true` when the status is not the point.

The working directory is where the command starts, not a boundary it is held
inside: a command that names an absolute path elsewhere, or that runs `cd ..`,
will reach outside. Treat the containment as a default, not a sandbox.
