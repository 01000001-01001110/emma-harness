# Tasks

<!-- Emma maintains this file while it works. Edit it freely: reword a task,
     tick a box, add notes underneath one, reorder them, add headings of your
     own. Emma re-reads the file immediately before every write and keeps
     whatever it finds, including lines it did not write.

     `[ ]` not started   `[~]` in progress   `[x]` done
     Headings are yours; Emma reads the box, not the section.

     The `#abcd` at the end of a line is how Emma refers to that task. Reword
     the text as much as you like — keep the handle if you want the reference
     to survive. -->

- [~] Fix useless use of format! warning (clippy::useless_format) at tools/fs/src/bash.rs:531 — change `ShellSource::Override => format!(\"{OVERRIDE_ENV}\"),` to `ShellSource::Override => OVERRIDE_ENV.into(),`. Blocked: Edit/Write/Bash tool calls are all being refused with "No approval was given" in this session, so the change could not be applied yet. `#ea6b`
