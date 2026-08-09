Read one web page and return it as markdown.

The page is rendered in a real browser, so pages built with JavaScript, and
pages that refuse plain HTTP requests, come back as content rather than as an
empty shell. Boilerplate — navigation, cookie banners, footers — is stripped.

- `url` must be `http://` or `https://`. Loopback addresses, `file:` and
  `chrome:` URLs are refused.
- `max_chars` bounds the page text. The default is 8000 characters; raise it
  for a long article. Whenever anything is cut the result says so and reports
  the full length, so a truncated page is never presented as a whole one.

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
