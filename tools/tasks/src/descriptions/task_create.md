Add one or more tasks to the project's task list and return their ids.

Write the whole plan in a single call, in the order you intend to do it. The
list is kept in `.emma/tasks/tasks.md`, a markdown file the person you are
working for can open and read while you work, so phrase each task as one plain
line they would recognise — "make the auth tests pass", not "step 3".

- `tasks` is an array. Each entry is either a string, or an object with `text`
  and an optional `status` of `pending`, `in_progress` or `completed`.
- New tasks default to `pending`. Mark exactly one `in_progress` when you start
  it, with `TaskUpdate`.
- The file and its directory are created if they do not exist.
- Ids come back as short handles like `#a3f1`. Pass them to `TaskGet` and
  `TaskUpdate`.

Creating tasks never removes or reorders anything already in the file,
including notes a human wrote under an existing task.
