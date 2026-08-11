Close a browser session and end its Chrome process. With no `session`, closes every session this run opened.

Call it as soon as a session is no longer needed. A live session is a few hundred megabytes of Chrome and, while it exists, a debugging port on localhost that any other local process can use to drive that browser — including whatever the session is logged into. Short sessions are the mitigation, and this is how they end.

Nothing is lost that matters: cookies and page state belong to a throwaway profile that is deleted with the browser. Anything worth keeping should already be in the transcript or in a file.

Closing an id that is not open is a result, not an error — it means the session is already gone.

You do not have to call it. Every session closes when the goal ends. Calling it is how the machine gets its memory back sooner.
