//! The one thing that owns a live Chrome, and the three mechanisms that make
//! sure none of them outlives this process.
//!
//! **This crate has leaked a Chrome before, and `tools/lsp` wrote it down.**
//! From `tools/lsp/src/lib.rs`: "`tools/web` leaked a Chrome per session
//! because its teardown ran in `main` — correct for a binary, wrong for a
//! library." Persistent sessions are that bug's natural habitat, so the
//! teardown here is deliberately three mechanisms rather than one, and each
//! covers a way the previous one does not run:
//!
//! 1. **[`BrowserPool::close`]** — the ordinary path. A polite CDP
//!    `Browser.close`, then a hard kill by pid, then the profile directory.
//! 2. **[`Drop`]** — normal exit and panic-unwind. `Drop` cannot `await`, so it
//!    kills by **pid**, which is the whole reason the pid is recorded at open
//!    time rather than being treated as a debugging detail.
//! 3. **[`BrowserPool::sweep_stale_profiles`]** at construction — the backstop
//!    for `abort()`, a SIGKILL, or a power cut, none of which run `Drop` at all.
//!
//! [`BrowserPool::kill_all_now`] is mechanism 2 exposed as a public, synchronous
//! call, for an interrupt handler that must not `await`. It is idempotent.
//!
//! **A leaked Chrome is not untidiness.** chromehand's own security note
//! (`session.rs:9–11`) is that a session's CDP websocket is unauthenticated on
//! localhost: while it lives, *any local process can puppet that browser*. A
//! Chrome that outlives Emma is a remote-control port left open on the user's
//! machine with a profile that may hold a login.
//!
//! # Why the session registry is in memory and not on disk
//!
//! chromehand records sessions in `.browser-miner/session-<id>.json`, relative
//! to the process's working directory, because two separate CLI *processes* have
//! to find each other's browsers. Emma is one process, and its working directory
//! is the user's repository — a file appearing there is the same class of
//! surprise [`crate::chromehand::load_policy`] refuses for the allowlist.
//!
//! So [`BrowserPool::open`] calls chromehand's `session::open` (which does the
//! genuinely hard part: a detached spawn with no inherited handles, and the CDP
//! endpoint discovery) and then **deletes the session file it wrote**, keeping
//! `{id, pid, ws_url, target_id}` here instead. The alternative was copying
//! sixty lines of `CreateProcessW` — including a Windows handle-inheritance scar
//! that cost somebody a debugging session — into a second place where it could
//! rot. A file that exists for a few hundred milliseconds and is removed beats a
//! duplicated scar.
//!
//! # What `host` is for
//!
//! A session's host **changes underneath it**: a click can navigate
//! cross-origin, and the approval gate reads a tool's destination from
//! [`emma_tool_api::Tool::network_target`] *before* the call, out of the call's
//! arguments — which say `session=abc`, not where that session now points. So
//! every verb records the `final_url` it ended on ([`BrowserPool::arrived_at`]),
//! and the browser tools answer `network_target` from **here** rather than from
//! their arguments. That is what makes a grant for one host stop covering a
//! session that has wandered to another.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::target::TargetId;
use futures::StreamExt;
use serde_json::Value;

use crate::chromehand::session::Connected;
use crate::chromehand::{self, MinerError, Policy};

// region: Limits, with the reasons attached
// ---------------------------------------------------------------------------
// Limits, with the reasons attached
//
// Two numbers and a directory name. Each of them is a decision rather than a
// tunable, so each says what it costs to change.
// ---------------------------------------------------------------------------

/// How many live browsers this process will hold at once.
///
/// A persistent Chrome is 200–400 MB resident for as long as the session lives.
/// Two of those on a laptop already running rust-analyzer is noticeable; three
/// is the point at which the user discovers the limit as swap. The third open is
/// **refused with the cap named**, so the model learns the rule instead of
/// discovering a slow machine.
pub const MAX_SESSIONS: usize = 2;

/// chromehand's idle TTL, kept as a *second* line of defence.
///
/// It is deliberately not the primary mechanism. A TTL that is doing the real
/// work means a Chrome lives up to half an hour past the process that wanted it;
/// `Drop` is what normally fires, and this only catches a session the model
/// opened and then forgot about inside one long run.
pub const SESSION_TTL: Duration = Duration::from_secs(30 * 60);

/// A profile directory untouched for this long belonged to a run that is gone.
///
/// Comfortably longer than [`SESSION_TTL`], because the failure mode of guessing
/// too low is deleting the profile of a browser that is still using it.
const STALE_PROFILE_AGE: Duration = Duration::from_secs(2 * 60 * 60);

/// chromehand's naming, which the sweep has to match to find anything.
const SESSION_PROFILE_PREFIX: &str = "browser-miner-session-";
/// `WebFetch`'s throwaway profiles — `browser-miner-profile-<pid>-<n>`. Swept
/// too, because the same abort that stranded a session stranded these.
const FETCH_PROFILE_PREFIX: &str = "browser-miner-profile-";

// endregion: Limits, with the reasons attached

// region: What the pool knows about one browser
// ---------------------------------------------------------------------------
// What the pool knows about one browser
//
// Everything needed to reconnect to it, kill it, and answer the approval gate's
// question about it. `host` is the field the cross-origin re-ask is built on;
// `delta_baseline` is why a ten-step session does not re-send the whole page
// every turn.
// ---------------------------------------------------------------------------

/// One live browser.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    /// The OS process. Recorded because `Drop` cannot `await` a polite close and
    /// a pid is the only teardown that works from a destructor.
    pub pid: u32,
    pub ws_url: String,
    pub target_id: String,
    pub headful: bool,
    /// Where the page is *now* — updated after every verb, not at open.
    pub url: String,
    /// The host of [`Session::url`], normalised. **This is what the approval
    /// gate is told**, and the reason a cross-origin navigation re-asks.
    pub host: String,
    pub opened_at: Instant,
    pub last_used: Instant,
    /// The previous read's inventory, for `compute_delta`. In memory rather than
    /// `.browser-miner/session-<id>.digest.json`, for the same reason the
    /// session record is.
    pub delta_baseline: Option<Value>,
}

impl Session {
    /// The profile directory chromehand gave this session. Derived rather than
    /// stored, because the derivation is chromehand's and must not drift into a
    /// second spelling here.
    pub fn profile_dir(&self) -> PathBuf {
        std::env::temp_dir().join(format!("{SESSION_PROFILE_PREFIX}{}", self.id))
    }

    pub fn idle(&self) -> Duration {
        self.last_used.elapsed()
    }
}

// endregion: What the pool knows about one browser

// region: The pool
// ---------------------------------------------------------------------------
// The pool
//
// A `std::sync::Mutex` and not a `tokio` one, on purpose: `Drop` and
// `kill_all_now` are synchronous and must be able to take it. Nothing in this
// file is `await`ed while it is held — every async path copies what it needs out
// and drops the guard first, which is also what keeps clippy's
// `await_holding_lock` quiet.
// ---------------------------------------------------------------------------

pub struct BrowserPool {
    sessions: Mutex<HashMap<String, Session>>,
    /// The interaction allowlist, or `None`. Home, never the project directory —
    /// see [`crate::chromehand::load_policy`].
    allowlist: Option<PathBuf>,
    /// Lifts the loopback refusal. See [`BrowserPool::for_fixture_tests`].
    allow_local: bool,
}

impl BrowserPool {
    /// Build the pool and sweep whatever a previous run stranded.
    pub fn new(allowlist: Option<PathBuf>) -> Self {
        Self::sweep_stale_profiles();
        Self {
            sessions: Mutex::new(HashMap::new()),
            allowlist,
            allow_local: false,
        }
    }

    /// A pool that will open `127.0.0.1` — **for offline fixture tests, and for
    /// nothing else.**
    ///
    /// The same escape hatch, with the same name and the same reasoning, as
    /// chromehand's own `--allow-local`: the agent's machine is not the web, and
    /// reaching it turns a page read into a way to probe services bound to
    /// localhost. It is a separate constructor rather than a parameter on
    /// [`BrowserPool::new`] so that no production call site can reach it by
    /// passing a `bool` — `web_tools` cannot type this name by accident, and a
    /// grep for it finds every use in one go.
    ///
    /// It does not lift the `file:`/`chrome:` scheme refusal, which chromehand's
    /// `Policy` enforces regardless.
    pub fn for_fixture_tests(allowlist: Option<PathBuf>) -> Self {
        let mut pool = Self::new(allowlist);
        pool.allow_local = true;
        pool
    }

    /// The allowlist path, for the tools that have to say whether one exists.
    pub fn allowlist(&self) -> Option<&Path> {
        self.allowlist.as_deref()
    }

    /// The policy this pool reads under: the allowlist if there is one, nothing
    /// if there is not.
    ///
    /// Reading is unrestricted without an allowlist; **interaction is not** —
    /// [`Policy::check_interaction`] refuses click/type/select outright when no
    /// allowlist is in force (chromehand's ADR-4: "reading the web is ordinary;
    /// acting on it is opt-in"). That asymmetry is the reason this returns a
    /// `Policy` rather than an `Option<Vec<String>>`: the two questions are
    /// already different methods on it and must not be re-derived here.
    pub fn policy(&self) -> Result<Policy, MinerError> {
        chromehand::load_policy(&chromehand::DigestOptions {
            allowlist: self.allowlist.clone(),
            allow_local: self.allow_local,
            ..chromehand::DigestOptions::default()
        })
    }

    pub fn ids(&self) -> Vec<String> {
        let live = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let mut ids: Vec<String> = live.keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn get(&self, id: &str) -> Option<Session> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Open a browser on `url`.
    ///
    /// The order is load-bearing. Policy is checked **before** Chrome starts, so
    /// a refusal costs nothing and cannot strand a process. The session is
    /// registered **before** the first navigation, so a navigation that fails
    /// still has something for the teardown path to find — the alternative is a
    /// spawned Chrome nobody is tracking, which is precisely the leak.
    pub async fn open(&self, url: &str, headful: bool) -> Result<Session, MinerError> {
        self.policy()?.check(url).map_err(MinerError::Refused)?;
        self.expire_idle().await;
        {
            let live = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            if live.len() >= MAX_SESSIONS {
                return Err(MinerError::Refused(format!(
                    "refused: {MAX_SESSIONS} browser sessions are already open ({}), which is the \
                     cap — each one is a few hundred megabytes of Chrome. Close one with \
                     BrowserClose before opening another",
                    live.keys().cloned().collect::<Vec<_>>().join(", ")
                )));
            }
        }

        let opened = chromehand::session::open(headful)
            .await
            .map_err(chromehand::classify)?;

        // Immediately, and before anything can fail: the file chromehand wrote
        // lives in the user's repository, and this pool is the registry.
        if let Some(file) = opened.get("session_file").and_then(Value::as_str) {
            forget_session_file(Path::new(file));
        }

        let id = opened
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let pid = opened.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
        let ws_url = opened
            .get("ws_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let target_id = opened
            .get("target_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if id.is_empty() || ws_url.is_empty() || target_id.is_empty() {
            // Nothing to register and possibly a process to kill. Fail loudly
            // rather than returning a session that cannot be torn down.
            if pid != 0 {
                kill_pid(pid);
            }
            return Err(MinerError::Browser(
                "chrome started but reported no usable session handle".into(),
            ));
        }

        let now = Instant::now();
        let session = Session {
            id: id.clone(),
            pid,
            ws_url,
            target_id,
            headful,
            url: String::new(),
            host: String::new(),
            opened_at: now,
            last_used: now,
            delta_baseline: None,
        };
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), session);

        // From here on every failure goes through `close`, because the browser
        // exists and something has to end it.
        let connected = match self.connect(&id).await {
            Ok(c) => c,
            Err(e) => {
                self.close(&id).await;
                return Err(chromehand::classify(e));
            }
        };
        let navigated = chromehand::actions::navigate(&connected, url).await;
        let final_url = navigated
            .as_ref()
            .ok()
            .and_then(|v| v.get("final_url").and_then(Value::as_str))
            .unwrap_or(url)
            .to_string();
        chromehand::session::disconnect(connected);
        if let Err(e) = navigated {
            self.close(&id).await;
            return Err(chromehand::classify(e));
        }
        self.arrived_at(&id, &final_url);
        self.get(&id).ok_or_else(|| {
            MinerError::Browser("the session vanished between opening and first read".into())
        })
    }

    /// Reconnect to a live session for the duration of one verb.
    ///
    /// chromehand's own `session::connect` reads the file this pool deleted, so
    /// this is the same reconnection against the in-memory record: the recorded
    /// websocket, and the recorded *target*, which is what keeps a verb on the
    /// tab the session opened rather than on whichever tab a popup made first.
    pub async fn connect(&self, id: &str) -> Result<Connected, String> {
        let s = self
            .get(id)
            .ok_or_else(|| format!("no such session '{id}'"))?;
        let (browser, mut handler) = Browser::connect(&s.ws_url)
            .await
            .map_err(|e| format!("session '{id}' unreachable ({e}) — close it and open another"))?;
        let handler_task = tokio::task::spawn(async move {
            while let Some(ev) = handler.next().await {
                if ev.is_err() {
                    break;
                }
            }
        });
        // Target discovery on a fresh connection is event-driven; the handler may
        // not know the recorded target yet. chromehand retries for five seconds
        // here for the same reason and this must not be reduced to one attempt.
        let deadline = Instant::now() + Duration::from_secs(5);
        let page = loop {
            match browser.get_page(TargetId::new(s.target_id.clone())).await {
                Ok(p) => break p,
                Err(e) => {
                    if Instant::now() >= deadline {
                        handler_task.abort();
                        return Err(format!("session '{id}' page target gone ({e})"));
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        };
        Ok(Connected {
            browser,
            handler_task,
            page,
            id: s.id,
            headful: s.headful,
            attached: false,
        })
    }

    /// Record where a verb left the session, and reset its idle clock.
    ///
    /// **This is the cross-origin re-ask.** Called after every verb, including
    /// the ones that were not supposed to navigate, because "was not supposed
    /// to" is exactly the case that matters: a click that turns out to be a link
    /// to another origin has to move the host the gate will be asked about.
    pub fn arrived_at(&self, id: &str, final_url: &str) {
        let host = host_of(final_url);
        let mut live = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = live.get_mut(id) {
            s.last_used = Instant::now();
            if !final_url.is_empty() {
                s.url = final_url.to_string();
            }
            if let Some(host) = host {
                s.host = host;
            }
        }
    }

    /// Store the baseline the next `delta` read will diff against.
    pub fn set_delta_baseline(&self, id: &str, snapshot: Value) {
        let mut live = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = live.get_mut(id) {
            s.delta_baseline = Some(snapshot);
        }
    }

    /// Close and forget one session. Safe to call on an id that is already gone.
    pub async fn close(&self, id: &str) -> Option<Session> {
        let s = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)?;
        // Polite first: a CDP `Browser.close` lets Chrome flush its profile,
        // which is the difference between a clean directory and one the removal
        // below silently fails on because a file is still mapped.
        if let Ok((mut browser, mut handler)) = Browser::connect(&s.ws_url).await {
            let pump = tokio::task::spawn(async move {
                while let Some(ev) = handler.next().await {
                    if ev.is_err() {
                        break;
                    }
                }
            });
            let _ = browser.close().await;
            pump.abort();
        }
        // …and the kill regardless, because "polite close returned Ok" is a
        // claim about a message being sent, not about a process being gone.
        kill_pid(s.pid);
        remove_profile(s.pid, &s.profile_dir());
        Some(s)
    }

    /// Close every session. What the end of a goal calls.
    pub async fn close_all(&self) -> Vec<String> {
        let ids = self.ids();
        let mut closed = Vec::new();
        for id in ids {
            if self.close(&id).await.is_some() {
                closed.push(id);
            }
        }
        closed
    }

    /// Kill every session **synchronously**, for a path that cannot `await` —
    /// `Drop`, and an interrupt handler.
    ///
    /// No polite close: there is no runtime to do it on. This is the mechanism
    /// that makes the pid worth recording, and it is idempotent, so an interrupt
    /// handler calling it and `Drop` calling it again is fine.
    pub fn kill_all_now(&self) {
        let doomed: Vec<Session> = {
            let mut live = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            live.drain().map(|(_, s)| s).collect()
        };
        for s in doomed {
            kill_pid(s.pid);
            remove_profile(s.pid, &s.profile_dir());
        }
    }

    /// Expire sessions nothing has touched for [`SESSION_TTL`].
    async fn expire_idle(&self) {
        let stale: Vec<String> = {
            let live = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            live.values()
                .filter(|s| s.idle() > SESSION_TTL)
                .map(|s| s.id.clone())
                .collect()
        };
        for id in stale {
            self.close(&id).await;
        }
    }

    /// Remove profile directories a previous run stranded.
    ///
    /// The backstop for the one exit `Drop` does not cover: `abort()`, a
    /// SIGKILL, a crash. Only age is consulted, and the threshold is hours
    /// rather than minutes, because the cost of guessing wrong is deleting the
    /// profile out from under a browser that is still using it. Removal failure
    /// is ignored on purpose — on Windows a live Chrome holds its own files
    /// open, so "the delete failed" is itself the signal that the directory was
    /// not ours to take.
    pub fn sweep_stale_profiles() -> usize {
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(SESSION_PROFILE_PREFIX) && !name.starts_with(FETCH_PROFILE_PREFIX)
            {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|m| {
                    SystemTime::now()
                        .duration_since(m)
                        .map(|age| age > STALE_PROFILE_AGE)
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if stale && std::fs::remove_dir_all(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

/// A registry entry with no browser behind it, for tests that are about the
/// bookkeeping rather than about Chrome.
///
/// `pid: 0` is the safety catch: [`kill_pid`] returns immediately on it, so a
/// test session can never kill a real process — pid 0 is not a process anyone
/// owns, and `taskkill /PID 0` would otherwise be a genuinely bad day.
#[cfg(test)]
pub fn test_session(id: &str, url: &str) -> Session {
    let now = Instant::now();
    Session {
        id: id.into(),
        pid: 0,
        ws_url: "ws://127.0.0.1:0/devtools/browser/none".into(),
        target_id: "none".into(),
        headful: false,
        url: url.into(),
        host: host_of(url).unwrap_or_default(),
        opened_at: now,
        last_used: now,
        delta_baseline: None,
    }
}

#[cfg(test)]
impl BrowserPool {
    pub fn insert_for_test(&self, s: Session) {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(s.id.clone(), s);
    }
}

/// **Mechanism 2.** Normal exit and panic-unwind.
///
/// `Drop` cannot `await`, which is the entire reason [`Session::pid`] exists.
/// The bug this replaces put teardown in `main`, where one early return skipped
/// it; here there is no path out of the process that leaves the pool alive.
impl Drop for BrowserPool {
    fn drop(&mut self) {
        self.kill_all_now();
    }
}

// endregion: The pool

// region: The pieces that are not chromehand's to lend
// ---------------------------------------------------------------------------
// The pieces that are not chromehand's to lend
//
// Two small functions that exist because the vendored equivalents are private,
// and the vendored modules are kept verbatim so a cherry-pick from canonical
// stays mechanical (`chromehand/mod.rs:3`). Both are short enough that copying
// beats a divergence.
// ---------------------------------------------------------------------------

/// Kill a process by pid, cross-platform, best effort.
///
/// Byte-for-byte chromehand's private `session::kill_pid`. Copied rather than
/// made `pub` there: ten lines duplicated is cheaper than a diff against
/// upstream, and this is the one place a rot between the two could not hurt —
/// `taskkill /T /F` is not going to change meaning.
fn kill_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output();
    }
}

/// Remove a session's profile directory once the browser that owned it is gone.
///
/// **Measured, not defensive.** After six live sessions the processes were all
/// gone and three profile directories were still on disk: a kill returns as soon
/// as the signal is delivered, and on Windows the files stay locked until the
/// process actually exits, so the removal that follows immediately fails. The
/// directory holds the session's cookies, which is exactly the thing not to
/// leave in a shared temp folder, so it is worth waiting out in teardown, where
/// there is nothing else to do.
///
/// **That was measured again and sharpened, in the sibling that had no retry.**
/// `chromehand::session::close` used the same kill-then-remove with nothing in
/// between and leaked a profile on 8 of 66 closes; instrumenting the leaks found
/// the killed Chrome still *running* every time. Hence
/// [`chromehand::session::wait_for_exit`], which is now the first thing here:
/// the retry below stops guessing at how long an exit takes and only covers the
/// renderer children, which exit on their own schedule after the parent does.
/// Neither half is redundant — the numbers for both are in `wait_for_exit`.
///
/// A failure even then is not fatal: `sweep_stale_profiles` on the next start
/// is the backstop's backstop, which is what it is for.
fn remove_profile(pid: u32, dir: &Path) {
    chromehand::session::wait_for_exit(pid);
    for attempt in 0..10 {
        if !dir.exists() || std::fs::remove_dir_all(dir).is_ok() {
            return;
        }
        // Not `tokio::time::sleep`: this runs from `Drop` too, where there is no
        // runtime to sleep on and nothing left to yield to.
        std::thread::sleep(Duration::from_millis(50 * (attempt + 1)));
    }
}

/// Delete the session record chromehand wrote into the working directory, and
/// the directory too if it is now empty.
///
/// `remove_dir` and not `remove_dir_all`: if anything else is in
/// `.browser-miner/` it belongs to somebody — a `browser-miner` CLI the user is
/// running themselves — and this code has no business removing it.
fn forget_session_file(file: &Path) {
    let _ = std::fs::remove_file(file);
    if let Some(dir) = file.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

/// The host of a URL, normalised the way [`emma_tool_api::NetworkTarget`] does.
///
/// Kept identical on purpose: the gate keys a session grant on `NetworkTarget`'s
/// spelling, and a pool that recorded `Example.COM` while the gate remembered
/// `example.com` would re-ask on every call — a prompt that fires when it should
/// not is how an operator learns to answer without reading.
pub fn host_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    Some(host.trim().trim_end_matches('.').to_ascii_lowercase())
}

// endregion: The pieces that are not chromehand's to lend

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Everything decidable without a browser. The guarantee that needs a real
// process — a Chrome never outlives the run that started it — is in
// `tests/browser_lifecycle.rs`, against a real pid, because a flag can be
// checked and a process cannot be argued with.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, url: &str) -> Session {
        let now = Instant::now();
        Session {
            id: id.into(),
            pid: 0,
            ws_url: "ws://127.0.0.1:0/x".into(),
            target_id: "t".into(),
            headful: false,
            url: url.into(),
            host: host_of(url).unwrap_or_default(),
            opened_at: now,
            last_used: now,
            delta_baseline: None,
        }
    }

    fn pool_with(sessions: Vec<Session>) -> BrowserPool {
        let pool = BrowserPool::new(None);
        {
            let mut live = pool.sessions.lock().unwrap();
            for s in sessions {
                live.insert(s.id.clone(), s);
            }
        }
        pool
    }

    /// The cross-origin mechanism, at the level it actually works: the pool's
    /// idea of where a session *is* has to move when a verb lands somewhere
    /// else, because that is the only thing `network_target` can read.
    #[test]
    fn a_navigation_moves_the_host_the_gate_will_be_asked_about() {
        let pool = pool_with(vec![session("a1", "https://example.com/start")]);
        assert_eq!(pool.get("a1").unwrap().host, "example.com");
        pool.arrived_at("a1", "https://evil.example/landing");
        assert_eq!(
            pool.get("a1").unwrap().host,
            "evil.example",
            "a session that navigated cross-origin still reports its original host, so a \
             grant for the first host silently covers the second"
        );
    }

    #[test]
    fn a_host_is_spelled_the_way_the_gate_spells_it() {
        // The gate keys its session grant on `NetworkTarget::new`'s
        // normalisation. Two spellings of one host is a re-prompt on every call.
        assert_eq!(
            host_of("https://Example.COM./x").as_deref(),
            Some("example.com")
        );
        assert_eq!(host_of("not a url"), None);
        assert_eq!(host_of("file:///etc/passwd"), None);
    }

    #[test]
    fn killing_everything_empties_the_registry_and_repeats_harmlessly() {
        // `kill_all_now` is called from `Drop` *and* from an interrupt handler,
        // so the second call must find nothing rather than kill a pid that has
        // been reused by an unrelated process in the meantime.
        let pool = pool_with(vec![
            session("a1", "https://x/"),
            session("b2", "https://y/"),
        ]);
        assert_eq!(pool.len(), 2);
        pool.kill_all_now();
        assert!(pool.is_empty());
        pool.kill_all_now();
        assert!(pool.is_empty());
    }

    #[test]
    fn a_profile_directory_is_derived_from_chromehands_own_naming() {
        // If these two spellings ever disagree, teardown removes nothing and
        // every run leaks a profile directory into the temp folder.
        let s = session("deadbeef", "https://x/");
        assert_eq!(
            s.profile_dir(),
            std::env::temp_dir().join("browser-miner-session-deadbeef")
        );
    }

    #[test]
    fn interaction_needs_an_allowlist_and_reading_does_not() {
        // chromehand's ADR-4, which this pool passes through rather than
        // reinterpreting. Reading the web is ordinary; acting on it is opt-in.
        let pool = BrowserPool::new(None);
        let policy = pool.policy().unwrap();
        assert!(policy.check("https://example.com/").is_ok());
        let refused = policy
            .check_interaction("https://example.com/")
            .unwrap_err();
        assert!(refused.contains("allowlist"), "{refused}");
    }

    #[test]
    fn the_session_file_is_removed_and_a_shared_directory_is_left_alone() {
        // The whole reason the registry is in memory: nothing of ours may be
        // left in the user's repository. And the directory is only removed when
        // it is empty — a `browser-miner` CLI the user runs themselves keeps its
        // own sessions there.
        let dir = tempfile::tempdir().unwrap();
        let nest = dir.path().join(".browser-miner");
        std::fs::create_dir_all(&nest).unwrap();
        let file = nest.join("session-abc.json");
        std::fs::write(&file, "{}").unwrap();
        forget_session_file(&file);
        assert!(!file.exists());
        assert!(!nest.exists(), "an empty session directory was left behind");

        std::fs::create_dir_all(&nest).unwrap();
        let ours = nest.join("session-abc.json");
        let theirs = nest.join("session-999.json");
        std::fs::write(&ours, "{}").unwrap();
        std::fs::write(&theirs, "{}").unwrap();
        forget_session_file(&ours);
        assert!(!ours.exists());
        assert!(theirs.exists(), "somebody else's session file was deleted");
    }
}

// endregion: Tests
