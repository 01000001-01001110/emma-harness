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
- `head_limit` **lowers** the number of returned lines or paths. The ceiling is
  500 and `head_limit` cannot raise it. When a search hits the ceiling, more
  lines are not available from this tool at all: narrow `pattern`, `path` or
  `glob`, or ask `output_mode=count` or `files_with_matches`, which answer how
  many and where without spending a line on each match.

Files that are not valid UTF-8 are skipped rather than searched as bytes — they
have no lines to match, so this is not reported. A **text** file larger than
8 MiB is not opened at all, which _is_ reported, because "no matches" would
otherwise be a claim about a file nothing looked inside. Matching lines longer than 400
characters are shown clipped, and the result says how many.

Finding nothing is a successful result with no matches, not an error — an empty
result means the pattern was not present, and only a malformed pattern or an
unreadable tree is an error.
