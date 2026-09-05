Search the web and get back a list of places to look.

- `query` is the search as a person would type it.
- `count` is how many results to return: 10 by default, at most 20.

Each result is a title and a URL, taken from a search engine's results page
rendered in a real browser. **A title is what the page says about itself, not
what it contains.** Use the list to choose which URL is worth reading, then read
it with `WebFetch`. Answering from a title is guessing; when a claim matters,
fetch the page and cite that.

No results is an answer, not a failure: the query found nothing. Rephrase, or
fetch a page directly if you already know where the information lives.

If the engine answers with a challenge page instead of results, this tool says
so and returns nothing else. That is the engine deciding this browser looks
automated, and it is not a fault in the query. The way through is
`BrowserOpen` on the same search URL with `headful: true`, then `BrowserRead`:
a visible browser window is the one a person can pass a challenge in, and the
results page reads the same way once it loads.
