Find files by path pattern, newest first.

- `pattern` is a glob matched against the path relative to the search root:
  `**/*.rs`, `src/**/mod.rs`, `Cargo.toml`.
- `path` optionally narrows the search to a subdirectory of the working
  directory. It defaults to the working directory itself.
- Results are files, not directories, sorted by modification time descending so
  the most recently touched work appears first.
- Paths listed in `.gitignore` are not searched by default, which is what keeps
  a search from spending its whole budget inside `target/`. Every result that
  skipped one says how many and names the first few.
- `include_ignored: true` searches them anyway, build output included.
- `.git` is never searched, under either setting.

Matching nothing is a successful result with an empty list, not an error.

At most 1000 paths are returned — the 1000 most recently modified — and **no
argument raises that**. When more matched, the result says how many and how many
were dropped. A pattern that matches tens of thousands of paths is usually
reaching into build output (`target/`, `node_modules/`, `dist/`); the answer is
a narrower `pattern` or a `path` inside the tree, not a second call.
