Fill several fields of a form in one call, addressed by the selectors `BrowserRead` listed.

Each entry needs a `selector` and either a `value` (text to type, or the option to choose in a dropdown) or `checked` (for a checkbox or radio). Every field is typed like a user so the page's validation fires, then re-read from the live page — the result tells you what is actually in each field, not what was sent.

**This never submits, and no tool here does.** Filling a form is reversible; submitting one buys something, sends something, or deletes something, and that needs a prompt showing the exact payload plus a key the user holds outside this conversation. Neither exists yet, so the capability is absent rather than half-guarded. If the form must go, open a `headful` session and let the user press the button themselves — they can see the real form while they decide, which is better than any prompt.

Two things are refused structurally and cannot be worked around: **password fields**, because this surface never handles credentials, and **file inputs**, because attaching a local file to a remote form is a different capability that is not offered here.

**This puts the user's data into somebody else's server, from their session.** The user is asked before each call and the prompt shows the field values verbatim. Filling is also restricted to the domains in their own `~/.emma/browser-allowlist.json`; with no such file it is refused.
