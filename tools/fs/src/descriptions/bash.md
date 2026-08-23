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
- Combined output is capped at 64 KiB per stream and **no argument raises it**;
  the result says so when output was cut, and what is shown is the start of each
  stream. For more than that, redirect the command's output to a file and then
  `Read` or `Grep` the file.
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
- `run_in_background: true` starts the command and returns at once with a task
  id (`bash_1`, `bash_2`, ...). **That result is a receipt for a start, not for
  a finish.** The command has not finished when the call returns: it has no
  exit status yet, none of its output is in the result, and it may already be
  failing. Do not report the work as done on the strength of the spawn — a
  build or test run "succeeds" this way while having done nothing. Call
  `BashOutput` with the task id to read output as it accumulates and, once the
  task exits, the real status; act on that status exactly as you would a
  foreground one. `KillShell` with the same id stops the command (its
  descendants may survive — nothing here kills a process tree).
- `timeout_ms` is refused together with `run_in_background` rather than
  silently ignored: a background command has no wait to bound, and runs until
  it exits or `KillShell` stops it.
- A background command is built exactly like a foreground one — same shell,
  same environment allowlist, same `cwd` rules. The only difference is who
  waits.

The working directory is where the command starts, not a boundary it is held
inside: a command that names an absolute path elsewhere, or that runs `cd ..`,
will reach outside. Treat the containment as a default, not a sandbox.
