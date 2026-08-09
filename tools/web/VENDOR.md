# The chromehand fork

`src/chromehand/` is a fork of **chromehand** (binary name `browser-miner`),
Alan's own project at `a sibling checkout`. Forked from commit `9c93827`
on **2026-08-09**, MIT (the upstream `Cargo.toml` declares `license = "MIT"`;
it ships no `LICENSE` file to copy).

**That repo is canonical and stays independent.** This copy is Emma's and is
free to diverge — it is not a vendored snapshot waiting to be re-synced, and
there is no automation pulling from upstream. If a fix lands there and matters
here, someone cherry-picks it by hand; the list below is what makes that
possible instead of archaeological.

## What changed in the fork

**Libification.** Upstream is binary-only: `main.rs` owned argv parsing, the
Chrome lifecycle (`launch_browser`/`teardown`), and every exit. Here the
lifecycle and the one command Emma needs live in [`chromehand::digest_url`],
and `src/bin/browser-miner.rs` is a thin CLI over it. Nothing inside the
vendored modules moved; the sole edit to those five files is `crate::x` →
`crate::chromehand::x`, applied mechanically.

**One unavoidable whitespace divergence.** `cargo fmt` runs over the whole
workspace, so the vendored files — `tests/integration.rs` most visibly — are
now formatted to Emma's settings rather than upstream's. A cherry-pick will
need `cargo fmt` run over it before the diff reads cleanly. Exempting the
directory was the alternative and it is worse: a corner of the workspace that
the formatter does not touch is a corner that quietly drifts.

**Exit codes became a type.** Upstream signalled outcome by process exit —
`0` result, `2` bad input or policy refusal, `3` browser failure. In-process
that is [`MinerError`], and the CLI turns it back into the same exit codes so
the vendored integration tests still mean what they meant. One distinction the
integers could not carry was added: Chrome being _absent_ is
`MinerError::Unavailable`, separate from Chrome _failing_, because Emma's tool
contract treats "I cannot do this" and "I tried and it broke" as different
facts the model routes on differently.

**Dropped, because Emma does not need them** (they remain in canonical):

- `adapters.rs` — per-site job-board extractors (`extract-jobs`). chromehand's
  own first use case, not a general web-reading capability. With it goes the
  one integration test that covered it, `extract_jobs_reports_blocked_not_stale`.
- `batch.rs` — `verify --file` fan-out, per-domain pacing, and the
  `browser-miner.stop` kill-switch file. The kill switch is cwd-relative
  process state with no meaning inside a library, and no test covered the
  batch path. Single-URL `verify` is unaffected: it lives in `digest.rs`.
- `bench/` — a live-network regression harness driven by Node. Emma's suite
  cannot depend on the network, and canonical is where those baselines belong.
- `docs/p*-certification.json` — chromehand's own certification history.

**Kept deliberately:** `policy.rs`, `session.rs`, `actions.rs` and `forms.rs`
in full, along with 21 of upstream's 22 integration tests — plus one written
to replace the dropped one, so the suite is 22 either way. That is where the
safety hardening lives — the localhost/scheme refusals, the allowlist
requirement on interaction verbs, `click` refusing submit controls, `type` and
`fill` refusing password fields, the newline-injection refusals, and the
two-key auto-submit rule. None of it is reachable from an Emma tool in this
pass, and it is here anyway: hardening that survives without its tests is
hardening nobody can trust later.
