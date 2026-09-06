Read one web page and return it as markdown.

The page is rendered in a real browser, so pages built with JavaScript, and
pages that refuse plain HTTP requests, come back as content rather than as an
empty shell. Boilerplate — navigation, cookie banners, footers — is stripped.

- `url` must be `http://` or `https://`. Loopback addresses, `file:` and
  `chrome:` URLs are refused.
- `max_chars` bounds the page text and nothing else. The default is 8000
  characters; raise it for a long article, up to 200000.
- `offset` skips that many characters of page text before the returned window
  starts. Default 0, the top of the page. When a read is cut, the result names
  the `offset` that continues it. Each continuation is a second fetch of the
  page — nothing is cached.
- `max_links` bounds how many links are listed, default 50 and capped at 120
  because the browser collects no more than 120 from one page. Raise it for an
  index, hub or search-results page, where the links are the content rather
  than the furniture.
- **The caps are independent, and the result always names the one that
  bound.** A truncation notice reports which limit cut, how many characters or
  links were dropped, and the exact call that returns the rest — `offset=N` for
  prose, `max_links=N` for links — so raising `max_chars` on a page whose
  _links_ were cut is a mistake the result tells you not to make. A truncated
  page is never presented as a whole one.

What comes back: the page's title, its final URL after redirects, the observed
HTTP status, the main-content text, any tables and JSON-LD, and the page's
links with main-content links first. Where a status could not be observed the
result says so rather than guessing one.

Some results are answers even though they look like failures, and should be
treated as answers:

- **A page with no readable text.** It rendered; its main content was an image,
  a video, an app shell or a redirect stub. Fetching it again will not differ.
- **A blocked page.** The site served an anti-bot challenge. That is the honest
  outcome, it is reported and never worked around, and retrying will not change
  it. Look for the information elsewhere.
- **A rendered 404 or error page.** The page exists and says the thing is gone.

This tool only reads. It cannot click, type, fill a form, or submit anything.
Use `WebSearch` first when you do not already know which URL to read.
