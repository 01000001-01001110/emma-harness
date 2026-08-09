Write `content` to `file_path`, creating the file or replacing it entirely.

**A file that already exists must have been read in full in this session
first.** A truncated or offset read does not count.
Overwriting a file whose contents you have not seen destroys work without ever
reporting a failure, so this tool refuses it and tells you to `Read` the file
first. The same refusal applies if the file changed on disk after you read it:
your picture of it is stale and the overwrite would silently discard whatever
changed.

To change part of a file, prefer `Edit`. `Write` replaces the whole file.

- Missing parent directories are created.
- `file_path` must resolve inside the working directory.
- Writing an empty string is allowed and truncates the file.
