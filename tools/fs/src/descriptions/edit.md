Replace an exact string in a file.

- `old_string` must appear **exactly once**. If it appears more than once the
  edit is refused and the count is reported — extend `old_string` with
  surrounding lines until it is unique, or pass `replace_all: true` if you mean
  every occurrence. There is no "first match" behaviour; which of several
  identical strings you meant is not something this tool will guess.
- `replace_all: true` replaces every occurrence and reports how many.
- `old_string` must match the file byte for byte, including indentation. Strip
  the line numbers `Read` adds before using its output here.
- The file must have been read in this session, for the same reason `Write`
  requires it: an anchor that matched a file you have not seen is a coincidence,
  not a location.

An `old_string` that is absent, or identical to `new_string`, is an argument
error. To create a new file, use `Write`.
