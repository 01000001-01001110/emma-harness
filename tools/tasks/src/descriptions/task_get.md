Return one task by id, including any notes written underneath it.

Use this when `TaskList`'s one-line summary is not enough — the notes are where
detail lives, whether you put it there or the person you are working for did.

- `id` is the handle shown by `TaskList`, such as `a3f1` or `#a3f1`.
- An id that is not in the file is an error, not an empty result: you named
  something that is not there. Call `TaskList` to see what exists.
- If there is no task list yet, every id is unknown; that is not a reason to
  stop, it means nothing has been written down.
