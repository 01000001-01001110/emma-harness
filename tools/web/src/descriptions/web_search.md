Search the web and get back a list of places to look.

- `query` is the search as a person would type it.
- `count` is how many results to return: 10 by default, at most 20.

Each result is a title, a URL, and a snippet. **The snippet is the search
engine's summary of a page, not the page.** It is written to help someone
decide whether to click, it is often stale, and it is sometimes wrong. Use the
snippets to choose which URL is worth reading, then read it with `WebFetch`.
Answering from a snippet is quoting a third party's paraphrase of a page nobody
opened; when a claim matters, fetch the page and cite that.

No results is an answer, not a failure — the query found nothing. Rephrase, or
fetch a page directly if you already know where the information lives.
