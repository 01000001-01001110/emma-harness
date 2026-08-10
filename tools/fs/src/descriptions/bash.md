Run a shell command.

The command is passed to a shell. **Which shell varies by machine, and every
result names it on the first line**, as `shell: <kind> — <path>`. POSIX is
the normal case, so pipes, redirection, `&&` and globbing work; an operator can
select PowerShell instead, and PowerShell does not have `&&`. Read the first
line before assuming the syntax, and if a command fails on syntax, look at it
again. The command runs in the working directory (or in `cwd`, which must be a
subdirectory of it).

- `timeout_ms` defaults to 120000 and is capped at 600000. On timeout the
  process is killed and the call fails, reporting whatever output was produced
  first.
- Combined output is capped at 64 KiB per stream; the result says so when
  output was cut.
- The environment is reduced to a fixed allowlist. It carries no API keys.
- **A command that ran is a success, whatever it exited with.** The result
  carries `exit status <n>` on its first line, then the output. A non-zero
  status is the world answering the question you asked: `grep -q` says "no
  match" with 1, `test -f` says "not there" with 1, `cargo test` says "some
  tests failed" with 101. Read the status and act on it.
- Do not append `|| true`. It throws away the answer and reports success for a
  command that failed.
- The call itself fails only when the command could not be started, timed out,
  or was killed — that is the shell being broken, not the command saying no.

The working directory is where the command starts, not a boundary it is held
inside: a command that names an absolute path elsewhere, or that runs `cd ..`,
will reach outside. Treat the containment as a default, not a sandbox.
