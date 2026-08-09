//! chromehand, in-process.
//!
//! Forked from `a sibling checkout` at commit `9c93827` (2026-08-09) — Alan's own
//! project, MIT, still canonical and still independent. See `VENDOR.md` for
//! what changed and what was dropped. The five modules below are upstream's
//! verbatim, with `crate::x` rewritten to `crate::chromehand::x` and nothing
//! else touched, so a later cherry-pick from canonical stays a mechanical
//! operation.
//!
//! **What this file is for.** Upstream had no library: `main` owned argv, the
//! Chrome lifecycle, and every exit. Everything in that shape that is not
//! argv parsing lives here now — launching the throwaway browser, tearing it
//! down, and turning failure into a value instead of a process exit.
//!
//! **The contract survives the move, and that is the point.** Upstream said
//! everything through exit codes: `0` a result *including honest negatives*,
//! `2` bad input or a policy refusal, `3` a browser failure. A page that is
//! blocked, a page with no readable text, a 404 — all of them exit 0, because
//! they are answers. [`MinerError`] carries exactly that taxonomy; the
//! honest-negative case is not in it, and must never be moved into it, because
//! `Ok` for "I looked and there was nothing" is the same rule `tool-api` is
//! built on.

pub mod actions;
pub mod digest;
pub mod forms;
pub mod policy;
pub mod session;

use std::path::PathBuf;

use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;

pub use digest::{DEFAULT_MAX_TEXT_CHARS, DEFAULT_TIMEOUT_MS};
pub use policy::Policy;

/// Everything that is not a result.
///
/// The three variants are upstream's three exit codes with one split: `3` used
/// to mean both "Chrome broke" and "there is no Chrome", and those are
/// different facts. A caller can retry the first. Nobody can retry the second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MinerError {
    /// Bad input, or the policy refused the URL — upstream's exit 2. A refusal
    /// is a *decision*, not a malfunction: `looks_blocked` never lands here,
    /// because a challenged page is a result the caller must be told about
    /// rather than an error it should route around.
    Refused(String),
    /// The browser or the navigation failed — upstream's exit 3.
    Browser(String),
    /// Chrome is not installed, or could not be started at all. Upstream had
    /// to spend exit 3 on this; in-process it is worth its own variant,
    /// because it is the one failure where retrying is pointless and the fix
    /// is an install.
    Unavailable(String),
}

impl MinerError {
    /// The `error` field upstream wrote into its JSON, kept stable so the CLI
    /// output and the schema still agree.
    pub fn stage(&self) -> &'static str {
        match self {
            Self::Refused(_) => "policy_refused",
            Self::Browser(m) if m.starts_with("timeout") => "timeout",
            Self::Browser(_) | Self::Unavailable(_) => "browser",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Refused(m) | Self::Browser(m) | Self::Unavailable(m) => m,
        }
    }
}

impl std::fmt::Display for MinerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.detail())
    }
}

impl std::error::Error for MinerError {}

/// Upstream's `die`: refusal-shaped error strings are exit 2, everything else
/// is a browser failure. Kept as one function rather than scattered, because
/// the classification is by prose and prose drifts — one place to fix when it
/// does.
pub fn classify(err: String) -> MinerError {
    if err.starts_with("refused")
        || err.contains("allowlist")
        || err.contains("expired")
        || err.starts_with("no such session")
        || err.starts_with("wait-for needs")
    {
        return MinerError::Refused(err);
    }
    MinerError::Browser(err)
}

/// How a page should be read. Upstream's flags, minus the ones that only made
/// sense as argv.
#[derive(Debug, Clone)]
pub struct DigestOptions {
    pub timeout_ms: u64,
    pub max_text_chars: usize,
    pub user_agent: Option<String>,
    /// Path to a `{"domains": [...]}` allowlist, or `None` to leave reading
    /// unrestricted. **Never resolved relative to a project directory by this
    /// crate** — see `crate::fetch` for where Emma looks and why.
    pub allowlist: Option<PathBuf>,
    /// Lifts the loopback refusal. Exists for offline fixture tests; it does
    /// not lift the `file:`/`chrome:` scheme refusal.
    pub allow_local: bool,
}

impl Default for DigestOptions {
    fn default() -> Self {
        Self {
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_text_chars: DEFAULT_MAX_TEXT_CHARS,
            user_agent: None,
            allowlist: None,
            allow_local: false,
        }
    }
}

/// Render one page and return the digest JSON — the same object upstream
/// printed on stdout, matching `docs/schema/chromehand-output.schema.json`.
///
/// The return type is `serde_json::Value` rather than a struct on purpose: the
/// schema *is* the contract, it is what the CLI emits and what the vendored
/// tests assert against, and a parallel set of Rust structs would be a second
/// definition of the same shape that can disagree with the first.
///
/// A blocked page, a 404 and a page with no text are all `Ok` here. That is
/// not laxity; it is the rule.
pub async fn digest_url(url: &str, opts: &DigestOptions) -> Result<serde_json::Value, MinerError> {
    let pol = load_policy(opts)?;
    pol.check(url).map_err(MinerError::Refused)?;

    let browser = LaunchedBrowser::launch(opts.user_agent.as_deref()).await?;
    let probed = digest::probe_page(&browser.browser, url, opts.timeout_ms).await;
    browser.teardown().await;
    let probe = probed.map_err(classify)?;
    Ok(digest::assemble(
        "digest",
        url,
        probe,
        true,
        opts.max_text_chars,
    ))
}

/// Liveness only — upstream's `verify <url>`, digest minus the payload.
pub async fn verify_url(url: &str, opts: &DigestOptions) -> Result<serde_json::Value, MinerError> {
    let pol = load_policy(opts)?;
    pol.check(url).map_err(MinerError::Refused)?;

    let browser = LaunchedBrowser::launch(opts.user_agent.as_deref()).await?;
    let probed = digest::probe_page(&browser.browser, url, opts.timeout_ms).await;
    browser.teardown().await;
    let probe = probed.map_err(classify)?;
    Ok(digest::assemble(
        "verify",
        url,
        probe,
        false,
        opts.max_text_chars,
    ))
}

/// **The library never picks an allowlist up off the working directory.**
///
/// Upstream's [`Policy::load`] falls back to `data/browser-allowlist.json`
/// relative to the process's cwd, which is right for a CLI a user runs from
/// their own project and wrong for a library whose cwd is whatever repository
/// Emma happens to be working in. A file in the repo silently changing which
/// domains are reachable is the same class of surprise as a credentials file
/// in the repo being read, and it is refused for the same reason — even though
/// this one can only ever restrict. Callers pass a path or get no allowlist;
/// the CLI still calls `Policy::load` itself and keeps upstream's behaviour.
pub fn load_policy(opts: &DigestOptions) -> Result<Policy, MinerError> {
    match &opts.allowlist {
        Some(path) => Policy::load(Some(&path.to_string_lossy()), opts.allow_local)
            .map_err(MinerError::Refused),
        None => Ok(Policy {
            allowlist: None,
            allow_local: opts.allow_local,
        }),
    }
}

/// A throwaway Chrome plus the two things that must be cleaned up with it: the
/// event-pump task, and the profile directory on disk.
///
/// Upstream returned a three-tuple and relied on `main` calling `teardown` on
/// every path. In a library that is a leak waiting for the one early return
/// nobody noticed, so the three live together and `teardown` consumes them.
pub struct LaunchedBrowser {
    pub browser: Browser,
    handler: tokio::task::JoinHandle<()>,
    profile_dir: PathBuf,
}

impl LaunchedBrowser {
    pub async fn launch(user_agent: Option<&str>) -> Result<Self, MinerError> {
        // Per-process *and* per-launch: two Emma turns fetching at once would
        // otherwise share one profile directory and the second launch would
        // find it locked.
        let profile_dir = std::env::temp_dir().join(format!(
            "browser-miner-profile-{}-{}",
            std::process::id(),
            next_profile_seq()
        ));
        let mut cfg = BrowserConfig::builder()
            .user_data_dir(&profile_dir)
            .arg("--disable-extensions")
            .arg("--no-first-run")
            .arg("--mute-audio");
        if let Some(ua) = user_agent {
            cfg = cfg.arg(format!("--user-agent={}", ua));
        }
        // A config that will not build is almost always "no Chrome executable
        // was found", which is an install problem and not a run-time fault.
        let cfg = cfg
            .build()
            .map_err(|e| MinerError::Unavailable(chrome_absent_message(&e)))?;
        let (browser, mut handler) = Browser::launch(cfg).await.map_err(|e| {
            let msg = e.to_string();
            if looks_like_chrome_absent(&msg) {
                MinerError::Unavailable(chrome_absent_message(&msg))
            } else {
                MinerError::Browser(format!("launch chrome: {}", msg))
            }
        })?;
        let handler = tokio::task::spawn(async move {
            while let Some(event) = handler.next().await {
                if event.is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            browser,
            handler,
            profile_dir,
        })
    }

    pub async fn teardown(self) {
        let Self {
            mut browser,
            handler,
            profile_dir,
        } = self;
        let _ = browser.close().await;
        let _ = browser.wait().await;
        handler.abort();
        let _ = std::fs::remove_dir_all(&profile_dir);
    }
}

fn next_profile_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Heuristic, and named as one. chromiumoxide reports a missing browser as an
/// ordinary spawn error, so the only signal is the message.
fn looks_like_chrome_absent(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("could not auto detect")
        || m.contains("no such file")
        || m.contains("not found")
        || m.contains("cannot find")
        || m.contains("system cannot find")
        || m.contains("os error 2")
}

fn chrome_absent_message(detail: &impl std::fmt::Display) -> String {
    format!(
        "Chrome or Chromium could not be found, so no page can be rendered \
         ({detail}). Install Google Chrome or Chromium, or set CHROME to the \
         executable path"
    )
}
