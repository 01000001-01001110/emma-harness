Start a browser session on a URL and return the page, with the CSS selectors needed to act on it.

A session is a real Chrome that stays open between calls, so a login, a cookie banner dismissal, or a search you have already typed survives to the next step. Use it when a task needs more than one interaction with the same site. For reading a single page, use `WebFetch` — it is cheaper and needs no cleanup.

Returns a session id, the page's text and links, and its interactive inventory: every field, button and link with the selector that reaches it. Pass those selectors verbatim to `BrowserAct` and `BrowserFill`; they are computed to be stable and a hand-written guess usually matches nothing.

**This is a different capability from reading the web.** Inside a session the model acts as the user: the browser can hold their login, and a click is a click from their account. Sessions are therefore short-lived by design — this one closes when the goal ends, or when you call `BrowserClose`, and it does not survive into another goal. Two sessions can be open at once; a third is refused, because each one is a few hundred megabytes of Chrome.

`headful: true` shows the window. Use it when the user needs to watch, or to do something themselves — filling in a password, or pressing a submit button, neither of which this surface will do.

Opening asks the user for approval twice over: once for the host, once because a live browser is a lasting change to their machine. Expect that and do not open speculatively.
