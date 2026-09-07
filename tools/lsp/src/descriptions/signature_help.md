The parameters of the call you are inside, and which one you are typing.

This answers "what does this function take, in order" at the point of the call
rather than at the point of the definition, which is the difference that matters
when the callee is generic or the receiver decides the overload. It also names
which argument the position is in, so a call being written can be checked
against the signature without counting commas.

Say where the point is by naming the file, the 1-based line, and `after` - the
text on that line immediately before the point you are asking about, which for a
call is usually the open bracket and whatever arguments are already typed:
`after: "take("` asks about the first argument, `after: "take(1, "` about the
second. If the text appears more than once on the line it refuses and asks for
`occurrence` rather than guessing.

- Rust files only. For any other language this refuses rather than guessing.
- `>` marks the signature the server considers active when it offered more than
  one.
- **Read the first two lines of every result.** They name the server and, when
  it was still indexing, say so. No signature from a server that was still
  indexing means the question could not be answered yet, not that the call has
  no signature.
