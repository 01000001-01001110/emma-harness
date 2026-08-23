Read what a background shell has produced since the last read.

- `bash_id` is the id a background `Bash` call returned, e.g. `bash_3`.
- Each call returns only what is new since the previous `BashOutput` call for
  that task, plus its state: still running, exited with a status, killed, or
  failed to run. Bytes returned once are not returned again.
- A finished task is still readable — its remaining output and its exit status
  stay available. "It finished" and "it never existed" are different answers,
  and only the second is an error.
- No new output is not an error. The result says which of two things is true:
  the task is still running and has said nothing since the last read, or it
  finished and there is nothing further to wait for.
- A background task keeps at most 256 KiB of output, oldest bytes dropped
  first. When anything has been dropped the result says how many bytes and
  that nothing returns them; reading more often, before the buffer wraps, is
  the only remedy.

An id no task in this session has is an error naming the ids that do exist.
