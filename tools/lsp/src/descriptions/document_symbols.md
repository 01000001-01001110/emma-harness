The shape of a Rust file: its types, functions, methods, constants and modules,
as a tree, with the line each one starts on.

Use this instead of reading a long file when you need to know what is in it and
where. It is the compiler's own outline, so it includes methods grouped under
their `impl` and gives each one its signature — which a `Grep` for `fn ` gives
you neither of.

It also gives you the line numbers that `FindReferences`, `GoToDefinition` and
`Hover` need.

- Rust files only. For any other language this refuses rather than guessing.
- Capped at 300 symbols; the result says so when it cut.
- **Read the first two lines of every result.** They name the server and, when
  it was still indexing, say so. An empty result from a server that was still
  indexing does not mean the file is empty.
