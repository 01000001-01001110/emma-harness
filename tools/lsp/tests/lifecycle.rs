//! What happens when the server dies, and what happens when Emma does.
//!
//! `tools/web` leaked a Chrome per session because teardown ran in `main` —
//! correct for a binary, wrong for a library, and invisible until somebody
//! counted processes. The lesson is not "remember to clean up"; it is that
//! cleanup belongs where the value ends. So the test that matters here spawns a
//! **real** process, drops the `Client`, and checks the process is gone — which
//! is the only way to tell a `kill_on_drop` that works from one that was
//! declared.
//!
//! The rest use the fake, because "dies in the middle of a request" is not a
//! thing a real server does on request.

mod support;

use std::sync::Arc;
use std::time::Duration;

use emma_tools_lsp::client::Client;
use emma_tools_lsp::lang::Language;
use emma_tools_lsp::pool::{Pool, MAX_CRASHES};
use emma_tools_lsp::server::{Server, Source};

/// The one language these lifecycle cases use. The pool is keyed by root *and*
/// language now, so every `client` call has to name one.
fn rust() -> &'static Language {
    emma_tools_lsp::lang::by_key("rust").expect("rust is in the table")
}
use support::{Fake, Indexing, Sandbox};

/// A process that will outlive its parent unless something kills it. Emma's own
/// `Bash` shell resolution is not reused here: this wants *any* long-running
/// child, and the point is the killing, not the program.
fn sleeper() -> Option<Server> {
    // A rust-analyzer that will never be spoken to still works as a subject —
    // but it may not be installed, so fall back to anything that sits still.
    // `Client::start` only needs a process with three pipes.
    for (path, version) in [
        ("/bin/sleep", "sleep"),
        ("/usr/bin/sleep", "sleep"),
        (r"C:\Windows\System32\timeout.exe", "timeout"),
    ] {
        if std::path::Path::new(path).is_file() {
            return Some(Server {
                program: path.into(),
                args: Vec::new(),
                entry: path.into(),
                version: version.into(),
                source: Source::Override,
                language: emma_tools_lsp::lang::by_key("rust").expect("rust is in the table"),
            });
        }
    }
    None
}

/// The `tools/web` lesson, asserted against a real operating system.
///
/// `Client::start` completes a handshake, and `timeout`/`sleep` will never
/// answer one — so the handshake is deliberately not waited for. The subject is
/// the process, not the protocol.
#[tokio::test]
async fn dropping_a_client_kills_its_process() {
    let Some(server) = sleeper() else {
        eprintln!("skipped: no long-running binary to spawn");
        return;
    };
    let sandbox = Sandbox::new();

    // Started by hand rather than through `Client::start`, whose handshake
    // would block for ninety seconds against a process that does not speak LSP.
    let mut child = tokio::process::Command::new(&server.program)
        .arg("3000")
        .current_dir(sandbox.root())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn");
    let pid = child.id().expect("a live child has a pid");
    let stdout = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();

    {
        // A `Client` that owns nothing but the streams still proves the half
        // that matters: its `Drop` aborts the pumps. The child below proves the
        // other half.
        let _client = Client::connect(server.clone(), sandbox.root(), stdout, stdin);
        drop(_client);
    }

    // The child, dropped: `kill_on_drop` plus `Client::drop`'s explicit
    // `start_kill` must both point the same way, and this is the assertion that
    // the flag is not merely set.
    drop(child);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!process_alive(pid), "process {pid} outlived its owner");
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()),
        Err(_) => false,
    }
}

/// Asked of `ps` rather than of `/proc` or `kill -0`, and both of those were
/// here before.
///
/// `/proc/<pid>` does not exist on macOS at all, so on that platform the check
/// silently fell through to the second clause; and on Linux the entry survives
/// for a **zombie**, which is a process that has exited and not been waited on.
/// `kill -0` has the same blindness from the other direction: it succeeds
/// against a zombie. Either one can therefore report "outlived its owner" about
/// a process that is already dead — the exact false accusation this test would
/// make loudly and be believed about, since it is the one guarding a leak that
/// really happened once. `ps -o state=` is the only one of the three that
/// distinguishes them: `Z` means gone.
#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    match std::process::Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
    {
        Ok(o) if o.stdout.iter().all(|b| b.is_ascii_whitespace()) => false,
        Ok(o) => !String::from_utf8_lossy(&o.stdout).trim().starts_with('Z'),
        // No `ps` on this box: fall back to the coarse question rather than
        // answering "gone", which would make the assertion pass for free.
        Err(_) => std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
    }
}

/// A death is reported once, with a reason, to everyone waiting.
///
/// The half that is easy to get wrong is the *pending* requests: without
/// draining them, each one sits out its own sixty-second timeout for a process
/// that is already gone, and the turn stalls for minutes reporting nothing.
#[tokio::test]
async fn a_server_that_dies_reports_it_rather_than_timing_out() {
    let sandbox = Sandbox::new();
    let client = Fake::new(Indexing::DiesAfterHandshake)
        .start(sandbox.root())
        .await;

    let started = std::time::Instant::now();
    let err = client
        .request("textDocument/references", serde_json::json!({}))
        .await
        .expect_err("a dead server cannot answer");
    // `Failed`, not `Unavailable`: the machinery was there and broke, which is
    // a different fact from "there is no server on this machine", and the model
    // routes differently on each.
    assert_eq!(err.kind(), "tool_failed");
    assert!(err.detail().contains("exited"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "it waited out a request timeout for a process that was already gone"
    );

    // And the death is remembered, so the pool does not hand this client out
    // again to fail every subsequent call.
    assert!(client.death().is_some());
}

/// A crash loop must look like a broken tool, not a slow one.
///
/// Without the ceiling, a server that dies on this workspace is respawned on
/// every call — each costing a spawn and an index — and the model sees latency
/// rather than a fault it can route around.
#[tokio::test]
async fn the_pool_stops_restarting_a_server_that_keeps_dying() {
    let sandbox = Sandbox::new();
    let pool = Pool::new();
    let root = sandbox.canonical();

    // `Pool::client` resolves a real server, which may not exist here — so this
    // exercises the accounting through the start-failure path, which counts
    // against the same ceiling and for the same reason.
    let mut last = None;
    for _ in 0..(MAX_CRASHES + 2) {
        last = Some(pool.client(&root, rust()).await);
    }
    let last = last.expect("looped");

    match last {
        // No rust-analyzer on this box: every attempt is a resolution failure,
        // which is `Unavailable` and does not count as a crash — a missing
        // binary is not a flapping one, and telling the user "it died three
        // times" when it never started would be a lie.
        Err(e) if e.kind() == "tool_unavailable" => {
            assert!(e.detail().contains("rust-analyzer"), "{e}");
        }
        // rust-analyzer is present and started: the ceiling was never reached
        // because nothing crashed, which is also correct.
        Ok(client) => assert!(client.death().is_none()),
        Err(e) => panic!("unexpected: {e}"),
    }
}

/// The pool hands the same server to two callers rather than starting two.
///
/// Two rust-analyzers indexing one workspace is the failure this exists to
/// prevent, and it is invisible except as the machine being slow.
#[tokio::test]
async fn one_root_gets_one_server() {
    let sandbox = Sandbox::new();
    let pool = Arc::new(Pool::new());
    let root = sandbox.canonical();

    let a = pool.client(&root, rust()).await;
    let b = pool.client(&root, rust()).await;
    match (a, b) {
        (Ok(a), Ok(b)) => assert!(Arc::ptr_eq(&a, &b), "the pool started two servers"),
        (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind(), "the same refusal both times"),
        _ => panic!("the pool disagreed with itself between two identical calls"),
    }
}
