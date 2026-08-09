# chromehand (browser-miner) — hands and eyes for ANY agent

**Harness-agnostic.** chromehand is a standalone CLI app, not a plugin for
anything: Claude Code, the Agent SDK, LangChain, CrewAI, a cron script — if
it can spawn a process and read stdout, it can browse. Point your agent at
this file (or paste it into its system prompt) and it knows how to drive the
web.

**The concept:** smaller footprint, less memory, fewer tokens — while giving
the agent FULL context of the page, and making page interactions faster and
more accurate than model-mediated browser tools. Use it instead of a
Playwright/MCP browser whenever the task is mechanical page work — read a
rendered page, inventory its elements, check liveness, fill a known field,
click a known control. Reach for a model-in-the-loop browser tool only when
you genuinely need pixels you must SEE, free-form visual exploration, or the
user's logged-in browser.

**The law: the binary is hands and eyes; you are the brain.** It renders,
digests, and executes exactly what you specify by selector. It never decides,
and you never parse raw HTML.

## Why use it (token economy)

An MCP browser round-trips accessibility trees/screenshots through the model
for every step. chromehand returns one compact JSON digest per call —
measured 4.6–5.9× smaller than raw HTML on live pages, with the interactive
inventory pre-extracted so there is no follow-up "find the apply link" pass.

## The binary

Built at `target/debug/browser-miner` (`cargo build`; needs Chrome
installed). Consumers may install to `data/bin/` or set `BROWSER_MINER_BIN`.
Every invocation prints ONE JSON object on stdout. Exit codes: `0` result
(including honest negatives), `2` bad input / policy refusal, `3` browser
failure. Full schema: `docs/schema/chromehand-output.schema.json`.

## Selectors can pierce shadow DOM and iframes

A digest selector may contain hop delimiters — pass it back to any action verb
verbatim; the binary resolves the hops for you:

- `>>>` descends an OPEN shadow root: `#my-app >>> #panel >>> button`.
  Elements inside carry `in_shadow: true`. Closed roots are unobservable.
- `|||` enters a SAME-ORIGIN iframe: `iframe#apply ||| #first_name`.
  Elements inside carry `in_frame: true`. Both delimiters compose.
- Cross-origin iframes can't be read; they're listed honestly under
  `digest.frames_unreadable` (`{selector, src}`) — never a content guess.

## THE CONTRACT NESTING (do not misparse)

Page content is NESTED under `digest.*` — `digest.text`,
`digest.interactive.{links,buttons,fields,forms}`, `digest.structured`,
`digest.meta`. Top-level holds `page_title`, `http_status`, `looks_blocked`,
`outcome`, `evidence`, `economy`. The first consumer assumed flat
`text`/`links` and misparsed; you have the schema, use it.

## Attach mode (ADR-2 — act as the user's own Chrome, opt-in and loud)

By default the binary launches its own throwaway Chrome. Attach mode connects
to a Chrome the USER already started with a debug port, acting as their
logged-in identity:

```bash
browser-miner session open --attach ws://127.0.0.1:PORT/...   # or --attach-port N
```

Every attach prints a consent banner to stderr (logged-in identity, site ToS
including LinkedIn/Wellfound, the Chrome-136 non-default-profile requirement,
no credential export). The user must have started Chrome themselves on a
non-default `--user-data-dir` — the binary never opens a debug port on a real
profile. **Closing an attached session never kills the user's Chrome** (it
just disconnects; the session records `managed: false`, `pid: 0`). Interaction
verbs still require the allowlist; password/submit refusals still apply.

## Reading (stateless — one page, one call)

```bash
browser-miner digest https://example.com          # full digest
browser-miner verify https://example.com          # liveness only, no payload
browser-miner verify --file urls.json --concurrency 3   # batch, paced per-domain
browser-miner screenshot https://example.com --out page.png
```

- `outcome` verdicts: `verified` (OBSERVED 2xx/3xx, not blocked) · `blocked`
  (anti-bot challenge — final answer, never evaded; do NOT retry with tricks)
  · `failed` (dead, rendered 404, or unobserved status — never upgraded
  without evidence).
- Batch kill switch: create `browser-miner.stop` in cwd → partial results,
  `stopped_early: true`.
- `--max-text-chars` (default 8000) bounds the text; `text_truncated` tells
  you honestly when content was cut.

## Acting (sessions — the agentic loop)

```bash
id=$(browser-miner session open | jq -r .id)
browser-miner navigate --session $id https://site.example/form   # observed status
browser-miner digest --session $id                               # read, pick selectors
browser-miner type  --session $id --selector "#name" --text "Ada Lovelace"
browser-miner select --session $id --selector "select[name=country]" --value "Canada"
browser-miner click --session $id --selector "#show-more"        # non-submit only
browser-miner wait-for --session $id --text "Loaded" --timeout-ms 8000
browser-miner digest --session $id                               # re-read new state
browser-miner screenshot --session $id --out evidence.png
browser-miner session close $id
```

- The loop is always **digest → decide (you) → act by selector → digest**.
- Interaction verbs (`click`/`type`/`select`) REQUIRE the user-owned
  allowlist (`data/browser-allowlist.json` `{"domains": [...]}` or
  `--allowlist <path>`). Reading is ordinary; acting is opt-in.
- `click` REFUSES submit-type controls (exit 2). Submission is the future
  two-key `submit` verb; do not try to work around the refusal.
- `type` REFUSES password fields. No credentials, ever.
- Sessions expire after 30 min idle; `session close --all` cleans up.
- In-session `digest` has `http_status: null` honestly (content read, not a
  navigation) — use `navigate`'s result for status evidence.

## digest-delta: only what changed (the loop token-saver)

In a session loop you re-digest after every action. `digest --session S
--delta` returns just what changed since the previous `--delta` on that
session — added/removed/changed elements keyed by selector, plus
`text_changed` (and the current text only when it changed). The first call is
the baseline (`delta.baseline: true`, full digest returned); every call after
diffs against it. Measured: a re-digest where nothing changed dropped from
~59 KB to ~0.6 KB (≈90× less). No fabrication — a missing snapshot is an
explicit baseline, never a guess.

```bash
browser-miner digest --session $id --delta   # 1st: baseline + full digest
browser-miner click  --session $id --selector "#show-more" --allowlist …
browser-miner digest --session $id --delta   # now: just the revealed region
```

## Keep-warm: skip the per-call Chrome launch

Stateless `digest <url>` launches and kills a Chrome per invocation (~2-3s
each). When you'll read more than 2-3 pages, open ONE session and reuse it —
`navigate` + `digest --session` skips the launch cost entirely:

```bash
id=$(browser-miner session open | jq -r .id)
for url in "$@"; do
  browser-miner navigate --session $id "$url" >/dev/null   # observed status here
  browser-miner digest --session $id                        # full digest, warm
done
browser-miner session close $id
```

Same isolated throwaway profile, same contract; liveness evidence comes from
each `navigate` result. Close the session when done — they expire after 30
minutes idle anyway, but don't leave a CDP socket listening longer than needed.

## Forms (extract-form → fill → submit)

```bash
browser-miner extract-form https://site.example/apply     # or --session $id
browser-miner fill --session $id --values values.json [--screenshot filled.png]
browser-miner submit --session $id --selector "#send"     # observe mode (see below)
```

- `extract-form` deepens the digest's field inventory: resolved labels,
  fieldset/legend groups, radio/checkbox groups collapsed to one logical
  field with per-option selectors, file inputs with `accept`, ARIA widgets,
  wizard signals (`wizard.likely`, next/prev buttons), and each form's
  submit controls. Password fields appear in the inventory with
  `fill_refused` — you can see them, you can never type into them.
- `values.json`: `{"fields": [{"selector": "#first_name", "value": "…"},
{"selector": "#remote_ok", "checked": true},
{"selector": "#resume", "file": "C:/path/resume.pdf"}]}` — selectors come
  from extract-form/digest; selects accept the value OR the visible option
  text. Every field is re-read live and reported `{requested, now_contains,
ok}`; unmatched selectors and password refusals are per-field results.
  **`fill` has no submit code path.** Multi-step wizards are the loop, not a
  feature: fill → `click` next → `wait-for` → `digest` → fill again.
- `submit` is its own verb under the **two-key rule**. Default is OBSERVE
  mode: it requires a `--headful` session, the HUMAN clicks submit, the
  binary records the outcome. Auto-submit works only when you pass
  `--yes-actually-submit` AND the user has personally set
  `"allow_auto_submit": true` (optionally `allow_auto_submit_domains`) in
  `data/browser-miner-config.json`. **You may not create or edit that file
  on the user's behalf — a refusal naming the missing key is the answer to
  relay, not an obstacle to remove.** Every submit is logged to
  `data/browser-miner-submit-log.jsonl`.

## Honesty rules you must respect

- `looks_blocked: true` is a FINAL answer. Report it. Never rotate UAs,
  retry-storm, or route around a challenge.
- `adapter_stale: true` (extract-jobs) means the site changed — report it as
  a maintenance signal, not an empty result.
- Every claim you make from a page should cite the digest's `evidence` block
  (real HTTP status from CDP, timestamp, final URL).

## Fallback etiquette

Binary absent → say so and fall back to your MCP browser tools; never treat
chromehand as a hard dependency. Chrome absent → exit 3 tells you; same
fallback.
