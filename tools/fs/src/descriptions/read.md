Read a text file from the working directory.

- `file_path` may be relative to the working directory or absolute, but must
  resolve inside it. Paths that escape are refused.
- Every line is labelled `<number>#<hash>`, then a tab, then the line:

  ```
      41#3c7e	fn handler(req: Request) -> Response {
      42#a3f9	    let user = legacy(req);
  ```

  The number is the 1-based line number. The hash is four hex characters
  standing for **exactly what that line says right now**, trailing spaces
  included. **Neither is part of the file.** Never include a label in an `Edit`
  `old_string` — but do copy it whole into `Edit`'s `lines`, which is the
  cheapest way to change a line: `"lines": "42#a3f9"` says "the line that reads
  `    let user = legacy(req);`", and the edit is refused rather than applied to
  the wrong place if that is no longer what line 42 says.

- A line too long to show whole is labelled `#----` instead of a hash. You have
  not seen the end of it, so `Edit` will not let you replace it by address.
- Reads at most 2000 lines and 256 KiB per call, and clips any single line
  longer than 2000 characters. When anything was cut the result says so
  explicitly and reports the line range that was returned; use `offset` to
  continue from there.
- `offset` is a 1-based line number. `limit` is a line count. Lines outside the
  window you read cannot be addressed by `Edit`; read them first.

Reading an empty file succeeds and returns no content. Reading a file that does
not exist, or a directory, is an argument error naming the path.
