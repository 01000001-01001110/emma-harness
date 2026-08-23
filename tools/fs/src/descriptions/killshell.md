Stop a running background shell.

- `bash_id` is the id a background `Bash` call returned, e.g. `bash_3`.
- The result says which of two things happened, and they are different facts:
  the shell was actually signalled and stopped, or it had already finished on
  its own and nothing was signalled. A task that already exited keeps its real
  exit status; killing it is a no-op, not a kill.

**This stops the shell Emma started, never a process tree.** A child that
detached — a server, a daemon, anything backgrounded with its own lifetime —
keeps running after the kill, because a deliberately daemonizing child is a
supported use and nothing can distinguish one from an orphan. If this task
launched a server you want gone, stop the server by its own mechanism; this
tool will not have done it.

Output the task produced before the kill remains readable with `BashOutput`.
An id no task in this session has is an error naming the ids that do exist.
