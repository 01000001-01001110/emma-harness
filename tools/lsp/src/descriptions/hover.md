The type and documentation of a symbol, as the compiler sees it.

This answers "what is this thing" without reading the file it is defined in: the
resolved type of a variable, the full signature of a function including
lifetimes and where-clauses, the doc comment attached to it. Inferred types —
what `let x = something()` actually is — are only available this way; they are
not written anywhere in the source for `Grep` to find.

Say where the symbol is by naming the file, the 1-based line, and the symbol as
it is written on that line. Emma finds the column itself; if the name appears
more than once on the line it refuses and asks for `occurrence` rather than
guessing.

- Rust files only. For any other language this refuses rather than guessing.
- **Read the first two lines of every result.** They name the server and, when
  it was still indexing, say so. "No type information" from a server that was
  still indexing means the question could not be answered yet, not that the
  symbol has no type.
