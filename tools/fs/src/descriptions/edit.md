Change part of a file. Say **where** in one of two ways — `lines` or
`old_string`. Sending both is accepted only when they name the same text; if
they disagree the edit is refused and the message quotes the addressed lines.

## `lines` — address by the label `Read` printed

`"lines": "42#a3f9"` means line 42, which must still say what it said when
`Read` showed it as `42#a3f9`. Copy the label from `Read`'s left-hand column
exactly; the hash is what makes this safe. For several lines at once give the
first and last, inclusive: `"lines": "42#a3f9-45#7b10"`.

`new_string` is the replacement text. It may be any number of lines. An empty
`new_string` **deletes** the addressed lines.

**If the line has changed since you read it the edit is refused, not applied.**
The refusal names the line, quotes what it says now, and — when that text has
simply moved — tells you which line it moved to, so you can re-address it
without reading the file again. This is the case to prefer whenever someone else
may have touched the file: your own editor, a formatter, a build step.

A successful edit reports the replacement lines **with their new labels**, and
says how far the lines below moved. Use those labels for your next edit; you do
not need to re-`Read` in between.

Lines you have not read cannot be addressed, and a line `Read` labelled `#----`
was too long to show whole, so it cannot be addressed either.

## `old_string` — quote the text

`old_string` must appear **exactly once**, matching byte for byte including
indentation. Strip the `<number>#<hash>` labels `Read` adds; they are not part
of the file.

- If it appears more than once the edit is refused and the count is reported —
  extend `old_string` with surrounding lines until it is unique, pass
  `replace_all: true` if you mean every occurrence, or address the one you mean
  with `lines`. There is no "first match" behaviour; which of several identical
  strings you meant is not something this tool will guess.
- `replace_all: true` replaces every occurrence and reports how many. It applies
  to `old_string` only.
- If the file changed on disk after you read it, `old_string` is refused
  outright — a quoted anchor can match text that moved somewhere it does not
  belong. Use `lines`, which is checked at a position, or `Read` again.

## Both forms

If you send `lines` **and** `old_string`, the address is what is used, and the
quote is only checked against it — so quoting the lines you just addressed is
harmless redundancy rather than an error. `Read`'s `<number>#<hash>` labels are
stripped before that comparison, so a quote copied straight out of `Read` still
agrees.

The file must have been read in this session, for the same reason `Write`
requires it: an anchor that matched a file you have not seen is a coincidence,
not a location.

An `old_string` that is absent, or identical to `new_string`, is an argument
error, as is a replacement that would leave the file byte for byte unchanged. To
create a new file, use `Write`.
