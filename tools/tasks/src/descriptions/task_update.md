Change a task's status, its wording, or both.

Keep the list honest as you work: mark a task `in_progress` when you start it,
`completed` the moment it is actually done — not when you plan to do it — and
reword it if what you are doing turned out to be different from what you wrote
down.

- `id` is the handle shown by `TaskList`. `status` is `pending`, `in_progress`
  or `completed`. `text` replaces the wording.
- At least one of `status` or `text` is required; a call that would change
  nothing is an error.
- An id that is not in the file is an error: you named something that is not
  there.

Completing a task ticks its box in place. It is not deleted and it does not
move, so the file stays a readable record of what was done and in what order,
and any note written under the task survives. If the list of completed tasks
gets long, that is for a person to prune; `TaskList` already leaves them out
unless you ask for them.

Marking work completed that is not completed is worse than leaving it open. The
list is what a person reads to find out where things stand.
