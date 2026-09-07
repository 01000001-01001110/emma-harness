What can be typed at a point in a file, ranked by the compiler.

This answers "what are the members of this thing" and "what is in scope here"
without reading the file the type is defined in. It is the tool to reach for
before writing a call you are not certain of: the list is the set of names that
actually exist at that point, so a name missing from it will not compile.

Say where the point is by naming the file, the 1-based line, and `after` - the
text on that line immediately before the point you are asking about. For members
of a value, include the dot: `after: "config."` asks what `config` has. Emma
puts the cursor at the end of that text and asks there. If the text appears more
than once on the line it refuses and asks for `occurrence` rather than guessing.

The line does not have to be complete or compile. Asking after `config.` on a
line that is still being written is the normal case and is what the server is
built for.

- Rust files only. For any other language this refuses rather than guessing.
- **The order is the server's, best first**, and it is worth more than the
  names: it encodes what is likely at this position. Do not re-sort it.
- Only the first 40 are shown. A completion list is narrowed by typing more of
  the word, not by asking for more of the list, and the result says so.
- **Read the first two lines of every result.** They name the server and, when
  it was still indexing, say so. An empty list from a server that was still
  indexing means the question could not be answered yet.
