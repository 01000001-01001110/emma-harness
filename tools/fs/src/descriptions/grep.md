Search file contents with a regular expression.

- `pattern` is a Rust regex (the same syntax as ripgrep), matched line by line.
- `path` optionally narrows the search to a file or subdirectory of the working
  directory.
- `glob` optionally filters which files are searched, e.g. `**/*.rs`.
- `case_insensitive` does what it says.
- `output_mode` is one of:
  - `content` (default) — matching lines, prefixed `path:line:`.
  - `files_with_matches` — one path per file containing a match.
  - `count` — `path:count` per file with at least one match.
- `head_limit` caps the number of returned lines or paths.

Files that are not valid UTF-8 are skipped rather than searched as bytes.
Finding nothing is a successful result with no matches, not an error — an empty
result means the pattern was not present, and only a malformed pattern or an
unreadable tree is an error.
