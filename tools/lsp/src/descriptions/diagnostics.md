Ask the language server what is wrong with one file: syntax errors, unresolved
names, type errors where the server checks types, and lint findings where it
runs a linter.

Use it straight after an `Edit` or a `Write`. This is the fastest honest answer
to "did I just break it", and it is the only tool here whose value comes from
being wrong-detecting rather than name-resolving.

Emma picks the server from the file's extension. There is no server for every
language; a file Emma has none for is refused by name rather than answered with
nothing.

**Read the first two lines of the result before believing the rest.** They name
the server and say whether it had finished indexing. Three outcomes look similar
and mean different things:

- a list of diagnostics: the server found these,
- "reported no problems": the server published an empty set, so this is a real
  clean bill of health for what that server checks,
- "published no diagnostics within Ns": the server did not answer. This is
  **not** a clean result. Nothing was ruled out. Wait and ask again.

A server that is still indexing says so on the second line, and a short answer
from it is not evidence of a clean file.

**What each server does and does not check.** Diagnostics are only as complete
as the analysis behind them, and the gaps are not the same per language:

- Rust: `cargo check` is deliberately switched off, because running it would
  mean a tool with no approval prompt compiling the repository under analysis.
  Measured with it off, what comes back is **syntax errors and nothing else**:
  a file whose only fault was a call to a function that does not exist came back
  as an explicit clean result, twice.
  So for Rust this answers "does it parse", and a clean answer here rules out
  nothing about names or types. Run `cargo check` with `Bash` for those; it is
  honest about being a build.
- Terraform and OpenTofu: provider-aware checks need schemas under
  `.terraform/`. In a directory that was never initialised the server knows the
  language and not the providers.
- Ansible: the deep checks come from `ansible-lint`. Without it installed, this
  is syntax and module names.
- Bash: lexical. Parse errors and unset variables, not semantics.

Line and column numbers in the result are 1-based, matching `Read` and `Grep`.
