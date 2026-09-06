//! chromehand, in-process.
//!
//! The five modules below are upstream's verbatim, with `crate::x` rewritten
//! to `crate::chromehand::x` and nothing else touched. Keep them that way: a
//! cherry-pick from canonical stays a mechanical operation only for as long as
//! that holds. `VENDOR.md` records the fork point and what was dropped.
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

pub use digest::{TextWindow, DEFAULT_MAX_TEXT_CHARS, DEFAULT_TIMEOUT_MS};
pub use policy::Policy;

// region: Failure as a value
// ---------------------------------------------------------------------------
// Failure as a value
//
// The whole of the libification, in one type. Upstream signalled outcome by
// process exit; in-process the same three facts have to survive as something a
// caller can match on, and the classification has to stay in one place because
// it is done by reading prose.
// ---------------------------------------------------------------------------

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

// endregion: Failure as a value

// region: How a page is read
// ---------------------------------------------------------------------------
// How a page is read
//
// Upstream's flags, minus the ones that only made sense as argv. Two of the
// five carry a decision rather than a preference — see each field.
// ---------------------------------------------------------------------------

/// How a page should be read.
#[derive(Debug, Clone)]
pub struct DigestOptions {
    pub timeout_ms: u64,
    pub max_text_chars: usize,
    /// Characters of page text to skip before the returned window starts.
    /// Zero is the head. See [`TextWindow`] for why this is a character index
    /// and what continuing actually costs.
    pub text_offset: usize,
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
            text_offset: 0,
            user_agent: None,
            allowlist: None,
            allow_local: false,
        }
    }
}

// endregion: How a page is read

// region: The two commands
// ---------------------------------------------------------------------------
// The two commands
//
// Everything Emma needs from a browser, and everything the CLI's stateless
// paths call. Both have the same shape — check policy, launch, probe, tear
// down, assemble — and both are `Ok` for every honest negative.
// ---------------------------------------------------------------------------

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

    // Policy runs before Chrome starts, so a refusal costs nothing and cannot
    // leave a browser behind. Below, note that `probed` is bound rather than
    // `?`-ed: teardown must happen on the failure path too, so the result is
    // held, the browser is torn down, and only then is the error propagated.
    // Swapping those two lines is the leak this shape exists to prevent.
    let browser = LaunchedBrowser::launch(opts.user_agent.as_deref()).await?;
    let probed = digest::probe_page(&browser.browser, url, opts.timeout_ms).await;
    browser.teardown().await;
    let probe = probed.map_err(classify)?;
    Ok(digest::assemble(
        "digest",
        url,
        probe,
        true,
        digest::TextWindow {
            offset: opts.text_offset,
            max_chars: opts.max_text_chars,
        },
    ))
}

/// Liveness only — upstream's `verify <url>`, digest minus the payload.
///
/// Reached from the CLI only. No Emma tool calls it, and `digest_md` cannot
/// render its output, which is missing the `digest` key by design.
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
        digest::TextWindow::head(opts.max_text_chars),
    ))
}

// endregion: The two commands

// region: The allowlist the library will not guess
// ---------------------------------------------------------------------------
// The allowlist the library will not guess
//
// One function, and the single sharpest divergence from upstream. It is short
// because the decision is the whole content.
// ---------------------------------------------------------------------------

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

// endregion: The allowlist the library will not guess

// region: Launch and teardown
// ---------------------------------------------------------------------------
// Launch and teardown
//
// A Chrome, an event-pump task and a directory on disk, which have to live and
// die together. Upstream could rely on `main` to clean up; a library cannot,
// and the two fixes for that — an owning struct, and a profile path unique per
// launch rather than per process — are the reason this section exists.
// ---------------------------------------------------------------------------

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

// endregion: Launch and teardown
