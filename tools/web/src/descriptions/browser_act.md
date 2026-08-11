Do one thing to the page a browser session is on: click, type, select, navigate, go back or forward, or wait for a condition.

- `click` — needs `selector`. Refuses submit-type controls; submitting a form is not something this surface does.
- `type` — needs `selector` and `text`. Types with real key events so the page's own validation fires. Password fields are refused.
- `select` — needs `selector` and `value`, matched against an option's value or its visible text.
- `navigate` — needs `url`. Same session, new page; cookies and login survive.
- `back` / `forward` — history.
- `wait_for` — waits for `selector` to appear, or for `text` on the page; with neither, waits for the network to go quiet, which is what you want after a click that fires a request. A timeout is an answer, not a failure.

**Selectors come from `BrowserRead`.** Use them exactly as listed.

The result is deliberately small: the URL you ended on, the page title, and whether the site served an anti-bot challenge. It never contains page content. Call `BrowserRead` to see what changed.

**This changes state on somebody else's server, as the user.** A click can post, delete, buy or send. The user is asked before each call, and acting is additionally restricted to the domains listed in their own `~/.emma/browser-allowlist.json` — with no such file, click, type and select are refused outright, and that is the user's decision to reverse, not yours. Reading the web is ordinary; acting on it is opt-in.
