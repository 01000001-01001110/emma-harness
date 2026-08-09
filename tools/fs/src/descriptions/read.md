Read a text file from the working directory.

- `file_path` may be relative to the working directory or absolute, but must
  resolve inside it. Paths that escape are refused.
- Output is line-numbered in `cat -n` style: a right-aligned line number, a tab,
  then the line. **The numbers are not part of the file.** Never include them in
  an `Edit` `old_string`.
- Reads at most 2000 lines and 256 KiB per call, and clips any single line
  longer than 2000 characters. When anything was cut the result says so
  explicitly and reports the line range that was returned; use `offset` to
  continue from there.
- `offset` is a 1-based line number. `limit` is a line count.

Reading an empty file succeeds and returns no content. Reading a file that does
not exist, or a directory, is an argument error naming the path.
