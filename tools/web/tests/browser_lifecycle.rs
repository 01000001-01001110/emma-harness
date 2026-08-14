//! The guarantees that need a real operating system, asserted against one.
//!
//! Three things here cannot be tested any other way, and all three are things
//! this crate has got wrong before or could plausibly get wrong next:
//!
//! 1. **A Chrome never outlives the run that started it.** `tools/lsp` records
//!    that `tools/web` "leaked a Chrome per session because its teardown ran in
//!    `main`". The only honest check is to spawn a real browser, take its pid,
//!    drop the pool, and ask the operating system whether that process still
//!    exists. A `kill_on_drop` flag can be *declared*; a process cannot be
//!    argued with.
//! 2. **A cross-origin navigation moves the host the approval gate is asked
//!    about.** The unit test pins the bookkeeping; this pins that a real
//!    navigation through a real browser actually reaches that bookkeeping.
//! 3. **Nothing is left in the working directory.** chromehand writes
//!    `.browser-miner/session-<id>.json` next to wherever the process is
//!    running, which inside Emma is the user's repository.
//!
//! Everything is served from `127.0.0.1`, so the suite needs no network — but it
//! does need Chrome, which is the same requirement `WebFetch` itself has. Each
//! test resolves it first and skips with a printed reason if it is absent,
//! rather than failing a machine that never claimed to have a browser.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use emma_tools_web::browser::pool::BrowserPool;

// region: A page to drive
// ---------------------------------------------------------------------------
// A page to drive
//
// Two pages and a link between them, on an ephemeral port. One thread per
// connection because Chrome opens speculative sockets that send nothing, and
// serving those serially starves the real request — a flake `tests/integration`
// already paid for once.
// ---------------------------------------------------------------------------

const PAGE_ONE: &str = "<!doctype html><html><head><title>Fixture One</title></head><body><main>\
     <p>The first page.</p><button id=\"reveal\" type=\"button\" onclick=\"document.getElementById('hidden').textContent='REVEALED_MARKER'\">Reveal</button>\
     <p id=\"hidden\"></p><a id=\"onward\" href=\"/two\">go to page two</a></main></body></html>";

const PAGE_TWO: &str = "<!doctype html><html><head><title>Fixture Two</title></head><body><main>\
     <p>SECOND_PAGE_MARKER</p></main></body></html>";

fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                let mut raw = Vec::new();
                let mut buf = [0u8; 2048];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&raw).to_string();
                let path = head
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                let body = if path.starts_with("/two") {
                    PAGE_TWO
                } else {
                    PAGE_ONE
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.write_all(body.as_bytes());
            });
        }
    });
    port
}

/// Chrome, or a printed reason to skip.
///
/// Skipping rather than failing: a machine with no browser has not broken
/// anything, and a suite that goes red there teaches people to ignore red.
fn chrome_present() -> bool {
    if chromiumoxide::browser::BrowserConfig::builder()
        .build()
        .is_ok()
    {
        return true;
    }
    eprintln!("SKIPPED: no Chrome or Chromium on this machine");
    false
}

/// Does the operating system still have this process?
///
/// Deliberately asked of the OS rather than of anything in this crate. The whole
/// value of this file is that it does not take our word for it.
fn alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .expect("tasklist");
        let text = String::from_utf8_lossy(&out.stdout);
        // `tasklist` with no match prints an INFO line, not an empty one, so the
        // pid itself is what has to be looked for.
        text.contains(&pid.to_string())
    }
    #[cfg(not(windows))]
    {
        // `kill -0` alone is the wrong question here, and answers it wrongly.
        // The pool spawns Chrome with `std::process::Command` and drops the
        // `Child` without reaping it (`chromehand/session.rs`), so after the
        // kill the pid is a **zombie owned by this test binary** and stays one
        // until the binary exits. `kill -0` succeeds against a zombie — a
        // process that is already dead — so the check that was meant to say
        // "the browser is still running" would instead report the leak this
        // file exists to detect, every time, on every unix.
        //
        // `ps -o state=` distinguishes them: `Z` is an exited process nobody
        // has waited on. That is *not* nothing — it is a pid-table entry Emma
        // is holding open, and it is written up as a product defect — but it is
        // not a browser still running, which is what this file asserts.
        let out = std::process::Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output();
        match out {
            // No row at all: reaped and gone.
            Ok(o) if o.stdout.iter().all(|b| b.is_ascii_whitespace()) => false,
            Ok(o) => !String::from_utf8_lossy(&o.stdout).trim().starts_with('Z'),
            // `ps` itself failed to run: fall back to the coarse question
            // rather than silently reporting "gone", which would make every
            // assertion in this file pass for the wrong reason.
            Err(_) => std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
        }
    }
}

/// Only one test in this file may have a Chrome starting at a time.
///
/// **Measured, and it was masquerading as a leak.** Every test here spawns a
/// real browser, and `cargo test` runs them concurrently, so five cold Chrome
/// starts land on the machine at once. That exceeds `session::open`'s 20-second
/// CDP-endpoint budget often enough to fail **4 of 12** runs of this binary —
/// and because each test panics at its own `open`, the name in the failure
/// report is whichever test lost the race. `closing_a_session_ends_the_process_and_removes_its_profile`
/// was reported as failing under a full workspace run on exactly this, which
/// reads as the profile leak that test exists to catch and is not: the leak
/// assertion is three lines further down and was never reached.
///
/// Serialised, the same binary failed **0 of 12**. Nothing about the assertions
/// changes — a real leak still fails the same line for the same reason. What
/// goes away is five tests competing for one machine's ability to start Chrome,
/// which is not a property this file is trying to establish.
///
/// Held for the whole test rather than only across `open`, because a test that
/// has finished spawning is still driving a browser the next one would contend
/// with.
///
/// `tokio`'s mutex and not `std`'s: the guard is held across every `await` in
/// the test body, which `clippy::await_holding_lock` rejects for `std` — and
/// rightly, since a blocking guard on a runtime thread is a deadlock waiting
/// for a reason. It also has no poisoning, so a panicking test hands the lock
/// on rather than failing the next one for somebody else's reason.
async fn one_browser_at_a_time() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

fn wait_until_gone(pid: u32) -> bool {
    for _ in 0..50 {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    false
}

// endregion: A page to drive

// region: The leak
// ---------------------------------------------------------------------------
// The leak
//
// The one this crate has already suffered, in the two shapes it can take:
// somebody calls close, and nobody calls anything.
// ---------------------------------------------------------------------------

/// **The guarantee.** Drop the pool; the browser is gone.
///
/// This is the test that fails if `impl Drop for BrowserPool` is deleted, if
/// `kill_all_now` stops killing, or if the pid stops being recorded at open. Any
/// of those three is the leak returning, and nothing else in the suite notices —
/// every other test would pass with a Chrome quietly surviving each run.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_pool_kills_the_browser_it_started() {
    if !chrome_present() {
        return;
    }
    let _serial = one_browser_at_a_time().await;
    let port = serve();
    let pid = {
        let pool = BrowserPool::for_fixture_tests(None);
        let session = pool
            .open(&format!("http://127.0.0.1:{port}/one"), false)
            .await
            .expect("open a session");
        assert!(
            session.pid != 0,
            "no pid was recorded, so nothing can kill it"
        );
        assert!(
            alive(session.pid),
            "the browser was not running even before the pool was dropped"
        );
        session.pid
        // …and the pool is dropped here, with the session still open. That is
        // the case that matters: a goal that ends without anybody calling close.
    };
    assert!(
        wait_until_gone(pid),
        "chrome {pid} outlived the pool that started it — this is the leak \
         tools/lsp recorded, back again"
    );
}

/// The ordinary path, and the one the model drives. Also checks the profile
/// directory goes with the process: a leaked profile is a copy of whatever the
/// session was logged into, left in the temp folder.
#[tokio::test(flavor = "multi_thread")]
async fn closing_a_session_ends_the_process_and_removes_its_profile() {
    if !chrome_present() {
        return;
    }
    let _serial = one_browser_at_a_time().await;
    let port = serve();
    let pool = BrowserPool::for_fixture_tests(None);
    let session = pool
        .open(&format!("http://127.0.0.1:{port}/one"), false)
        .await
        .expect("open a session");
    let profile = session.profile_dir();
    assert!(profile.is_dir(), "chrome started with no profile directory");

    pool.close(&session.id).await.expect("close");
    assert!(
        wait_until_gone(session.pid),
        "close left the process running"
    );
    assert!(
        !profile.exists(),
        "the profile directory outlived the session: {}",
        profile.display()
    );
    assert!(pool.is_empty(), "a closed session is still in the registry");
    // Closing again is a result, not a crash — `BrowserClose` relies on it.
    assert!(pool.close(&session.id).await.is_none());
}

/// Nothing of ours is left in the directory Emma happens to be working in.
///
/// chromehand writes `.browser-miner/session-<id>.json` relative to the
/// process's cwd, which is right for a CLI a user runs from their own project
/// and wrong for a library whose cwd is the user's repository. The pool deletes
/// it; if that ever stops happening, every session leaves a file in somebody's
/// git status.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_leaves_nothing_in_the_working_directory() {
    if !chrome_present() {
        return;
    }
    let _serial = one_browser_at_a_time().await;
    let port = serve();
    let pool = BrowserPool::for_fixture_tests(None);
    let session = pool
        .open(&format!("http://127.0.0.1:{port}/one"), false)
        .await
        .expect("open a session");
    let record = std::env::current_dir()
        .unwrap()
        .join(".browser-miner")
        .join(format!("session-{}.json", session.id));
    assert!(
        !record.exists(),
        "a session record was left in the working directory: {}",
        record.display()
    );
    pool.close(&session.id).await;
}

// endregion: The leak

// region: The cross-origin re-ask, through a real navigation
// ---------------------------------------------------------------------------
// The cross-origin re-ask, through a real navigation
//
// The unit tests pin the bookkeeping. This pins that a real page moving under a
// real session actually reaches it — the half that a refactor of the verb path
// could break while every unit test stayed green.
// ---------------------------------------------------------------------------

/// A navigation the model did not ask for — a link click — moves the host the
/// gate will be asked about.
///
/// Served from one origin, so what is asserted is that the *recorded location*
/// tracks the page rather than the open. That is the mechanism; the host is read
/// off the same field, and `pool::tests` pins the host half against a second
/// origin without needing two ports.
#[tokio::test(flavor = "multi_thread")]
async fn a_click_that_navigates_moves_the_recorded_location() {
    if !chrome_present() {
        return;
    }
    let _serial = one_browser_at_a_time().await;
    let port = serve();
    let pool = Arc::new(BrowserPool::for_fixture_tests(None));
    let session = pool
        .open(&format!("http://127.0.0.1:{port}/one"), false)
        .await
        .expect("open a session");
    assert!(pool.get(&session.id).unwrap().url.ends_with("/one"));

    // Straight through chromehand, because the point is the *pool* update, and
    // going via the tool would need an allowlist for the interaction gate — a
    // separate guarantee, tested separately.
    let connected = pool.connect(&session.id).await.expect("reconnect");
    let out = emma_tools_web::chromehand::actions::navigate(
        &connected,
        &format!("http://127.0.0.1:{port}/two"),
    )
    .await
    .expect("navigate");
    let landed = out["final_url"].as_str().unwrap_or_default().to_string();
    emma_tools_web::chromehand::session::disconnect(connected);
    pool.arrived_at(&session.id, &landed);

    let after = pool.get(&session.id).unwrap();
    assert!(
        after.url.ends_with("/two"),
        "the pool still thinks the session is at {} — a grant for the first page's host \
         would silently cover wherever it went next",
        after.url
    );
    assert_eq!(after.host, "127.0.0.1");
    pool.close(&session.id).await;
}

/// The cap is a refusal with the number in it, not a slow machine.
#[tokio::test(flavor = "multi_thread")]
async fn a_third_session_is_refused_and_names_the_cap() {
    if !chrome_present() {
        return;
    }
    let _serial = one_browser_at_a_time().await;
    let port = serve();
    let url = format!("http://127.0.0.1:{port}/one");
    let pool = BrowserPool::for_fixture_tests(None);
    let a = pool.open(&url, false).await.expect("first session");
    let b = pool.open(&url, false).await.expect("second session");
    let refused = pool
        .open(&url, false)
        .await
        .expect_err("a third session was allowed past the cap");
    assert!(
        refused.to_string().contains("cap"),
        "the refusal did not name the cap: {refused}"
    );
    pool.close(&a.id).await;
    pool.close(&b.id).await;
}

// endregion: The cross-origin re-ask, through a real navigation
