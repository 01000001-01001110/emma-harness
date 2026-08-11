Find where a symbol is actually defined, using the compiler's understanding of
the code rather than text matching.

Unlike a `Grep` for `fn name` or `struct Name`, this resolves imports, traits
and re-exports: it answers where _this_ symbol comes from, not where something
with that name is written.

Say where the symbol is by naming the file, the 1-based line, and the symbol as
it is written on that line. Emma finds the column itself; if the name appears
more than once on the line it refuses and asks for `occurrence` rather than
guessing.

- Rust files only. For any other language this refuses rather than falling back
  to a text search.
- The result is the file, the line number and the source line, which is often
  the whole answer.
- A definition in a dependency or the standard library is outside the working
  directory: it is counted and named, not shown.
- **Read the first two lines of every result.** They name the server and, when
  it was still indexing, say so. An empty result from a server that was still
  indexing is not "this has no definition" — the result says which of the two it
  is. When it says the index was incomplete, wait a few seconds and ask again.
