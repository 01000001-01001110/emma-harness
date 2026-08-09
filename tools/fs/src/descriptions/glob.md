Find files by path pattern, newest first.

- `pattern` is a glob matched against the path relative to the search root:
  `**/*.rs`, `src/**/mod.rs`, `Cargo.toml`.
- `path` optionally narrows the search to a subdirectory of the working
  directory. It defaults to the working directory itself.
- Results are files, not directories, sorted by modification time descending so
  the most recently touched work appears first.
- `.git` is not searched.

Matching nothing is a successful result with an empty list, not an error. At
most 1000 paths are returned; when there were more, the result says so.
