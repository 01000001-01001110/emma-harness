Read the page a browser session is currently on — the text, the links, and every element's selector.

By default this returns a **delta**: what changed since the last read, keyed by selector, so a click that opened a menu reports the menu appearing rather than reporting that the whole page moved. That is what makes a ten-step session affordable. Pass `delta: false` for the whole page — worth doing after a navigation, or when you have lost track of where you are.

`selectors` filters the interactive listing to elements whose selector, label, name or type contains the text you give: `"search"`, `"email"`, `"submit"`. A busy page has a hundred addressable elements and you usually want one of them.

Nothing here changes the page. It reads the DOM as it stands, so it is safe to call between actions and it is the intended way to check what an action actually did — `BrowserAct` returns only a URL and a title, never content.

The session must already be open (`BrowserOpen`). If it has navigated to another host since it was opened — a redirect, or a link that led somewhere else — the user is asked again before the new host's content is read. A grant covers a host, not a session.
