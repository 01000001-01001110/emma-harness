List the project's tasks, newest section of the file last, one line each.

The first line is always the counts — pending, in progress, completed — for the
whole file, so a single glance answers whether anything is still outstanding.

- `status` filters. It defaults to `open`, which is `pending` and
  `in_progress` together. Pass `all` to include completed tasks, or a single
  status to see just those.
- Output is one line per task: the checkbox, the handle, and the text. Notes
  written under a task are not included here; use `TaskGet` for those.
- No tasks, no file yet, or nothing matching the filter are all ordinary
  results, not errors.

The human you are working for can edit this file while you work: reword tasks,
tick boxes, add or delete lines. Whatever this returns is what the file says
now, not what you last wrote.
