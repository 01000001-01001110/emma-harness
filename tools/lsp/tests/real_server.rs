//! The tests that need an actual rust-analyzer.
//!
//! Everything else in this suite runs against a fake, which is right — the
//! properties that matter are properties of this crate. But a fake cannot tell
//! you whether the handshake is one rust-analyzer accepts, whether the
//! initialization options are spelled the way it reads them, whether it emits
//! the progress tokens readiness is keyed on, or whether a `didOpen` plus a
//! position produces the references you asked for. Those are agreements with
//! another program, and only that program can confirm them.
//!
//! **They skip rather than fail when no server is installed**, and say so on
//! stderr. A suite that cannot run on a machine without rust-analyzer would
//! simply be deleted; one that quietly passes there would be worse. The skip
//! prints, so "it passed" and "it did not run" are distinguishable in a log.
//!
//! The fixture is its own tiny crate in a temp directory rather than this
//! workspace: indexing Emma takes far longer, `--offline` makes it dependent on
//! what happens to be in the cargo cache, and a test whose subject is the code
//! under test is a test that changes meaning every time somebody edits it.

mod support;

use std::time::Duration;

use emma_tool_api::Tool;
use emma_tools_lsp::client::Readiness;
use emma_tools_lsp::{server, DocumentSymbols, FindReferences, GoToDefinition, Hover, Pool};
use serde_json::json;
use std::sync::Arc;
use support::Sandbox;

const LIB_RS: &str = r#"//! A fixture crate.

/// Configuration for the thing.
pub struct Config {
    pub name: String,
}

impl Config {
    pub fn new(name: String) -> Config {
        Config { name }
    }
}

pub fn describe(config: &Config) -> String {
    // Config appears in this comment too, which grep would count and a
    // language server will not.
    format!("config {}", config.name)
}

pub fn build() -> Config {
    Config::new("default".to_string())
}
"#;

const CARGO_TOML: &str = "[package]\nname = \"lsp-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n";

/// Sets up a real server over a one-file crate, or `None` if there is none
/// installed.
async fn fixture() -> Option<(Sandbox, Arc<Pool>)> {
    match server::resolve() {
        Ok(found) => eprintln!("real-server tests running against {found}"),
        Err(e) => {
            eprintln!("SKIPPED: no rust-analyzer on this machine — {e}");
            return None;
        }
    }
    let sandbox = Sandbox::new();
    sandbox.write("Cargo.toml", CARGO_TOML);
    sandbox.write("src/lib.rs", LIB_RS);
    let pool = Arc::new(Pool::new());
    // Start it here so the first tool call is not also the one paying for the
    // spawn; the point of the test is the answers, not the latency.
    pool.client(&sandbox.canonical()).await.ok()?;
    Some((sandbox, pool))
}

/// The whole crate, against the real thing: it starts, it reports progress, it
/// reaches `Ready`, and it answers a reference query with the uses and not the
/// comment.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_rust_analyzer_indexes_and_answers() {
    let Some((sandbox, pool)) = fixture().await else {
        return;
    };
    let client = pool.client(&sandbox.canonical()).await.expect("started");

    // Readiness first, and this is the assertion that the progress tokens
    // `client::is_indexing_token` matches are the ones rust-analyzer actually
    // sends. Get that wrong and every answer is `Unknown` forever — the crate
    // still works, and its central promise degrades to a permanent shrug that
    // nothing else here would notice.
    let readiness = tokio::time::timeout(Duration::from_secs(180), client.wait_ready())
        .await
        .expect("wait_ready has its own ceiling and must not hang");
    assert_eq!(
        readiness,
        Readiness::Ready,
        "rust-analyzer did not reach Ready — either the progress tokens changed \
         or the handshake was not accepted"
    );

    // `Config` is declared on line 4, used on 8, 9, 10, 14, 20, 21 — and
    // mentioned in a comment on line 15, which is the one grep gets wrong and
    // this must not return.
    let out = FindReferences::new(pool.clone())
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/lib.rs", "line": 4, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("no tool error");
    eprintln!("--- FindReferences ---\n{}", out.content);

    assert!(out.content.contains("src/lib.rs"), "{}", out.content);
    assert!(
        !out.content.contains("there are none"),
        "a real workspace with six uses answered none: {}",
        out.content
    );
    // The line the comment is on. If it appears, the tool is doing text search
    // by some other name.
    assert!(
        !out.content.contains("which grep would count"),
        "a comment was returned as a reference: {}",
        out.content
    );
    assert!(out.content.contains("pub fn describe"), "{}", out.content);
    assert!(out.content.contains("pub fn build"), "{}", out.content);
}

/// Definition, hover and symbols against the real server. Grouped into one test
/// because each costs an index and they can share one.
#[tokio::test(flavor = "multi_thread")]
async fn definition_hover_and_symbols_answer_for_real() {
    let Some((sandbox, pool)) = fixture().await else {
        return;
    };

    // `Config` used inside `describe`, resolving back to the declaration.
    let out = GoToDefinition::new(pool.clone())
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/lib.rs", "line": 14, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("no tool error");
    eprintln!("--- GoToDefinition ---\n{}", out.content);
    assert!(out.content.contains("src/lib.rs"), "{}", out.content);
    assert!(
        out.content.contains("4: pub struct Config"),
        "{}",
        out.content
    );

    let out = Hover::new(pool.clone())
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/lib.rs", "line": 4, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("no tool error");
    eprintln!("--- Hover ---\n{}", out.content);
    assert!(out.content.contains("Config"), "{}", out.content);
    // The doc comment: the thing hover gives you that nothing else does.
    assert!(
        out.content.contains("Configuration for the thing"),
        "{}",
        out.content
    );

    let out = DocumentSymbols::new(pool)
        .invoke(&sandbox.ctx, json!({ "file_path": "src/lib.rs" }))
        .await
        .expect("no fault")
        .expect("no tool error");
    eprintln!("--- DocumentSymbols ---\n{}", out.content);
    for expected in ["Config", "new", "describe", "build"] {
        assert!(
            out.content.contains(expected),
            "{expected}: {}",
            out.content
        );
    }
}

/// `read_only: true`, measured against the thing it is a claim about.
///
/// The fake-server version of this test (`tests/tools.rs`) proves the *tool*
/// writes nothing, which is worth having and is not the interesting half: the
/// risk is not Emma's code, it is rust-analyzer's. Left to its defaults it
/// runs `cargo check` on save, executes the analysed project's `build.rs`,
/// and builds its proc macros — a compiler writing megabytes into `target/`,
/// started by a tool the approval gate waves through. `client::INIT_OPTIONS`
/// turns all three off, and `client::tests::the_dangerous_switches_are_off`
/// pins the spelling.
///
/// The one write the server does make, measured rather than assumed:
/// rust-analyzer builds the crate graph by running `cargo metadata`, and cargo
/// materializes `Cargo.lock` whenever it is missing or out of date with
/// `Cargo.toml` — the same write `cargo build` makes on first run. The
/// original version of this test asserted "no file added, not even a
/// `Cargo.lock`", and that was a fact about the servers it happened to be
/// measured against:
///
/// - 2026-08-11, 0.3.3008: a full index and a satisfied reference query added
///   no file — not even a `Cargo.lock`, which a bare `cargo metadata --offline`
///   in the same directory does create.
/// - 2026-09-02, 0.3.3033 (0.3.3025 still writes nothing): `Cargo.lock` is
///   created when absent, refreshed when stale, and left byte-identical when
///   valid; nothing else in the project changes, with dependencies or without,
///   and no `target/` appears.
///
/// Whether that write fits the bit is a question about what `read_only` asks,
/// and [`emma_tool_api::ToolMeta`]'s own documentation answers it: the bit
/// asks "can this damage this machine", not whether every byte stays put —
/// the reading that lets `WebFetch` declare it while driving a browser. A
/// lockfile carries no project content. There is also no rust-analyzer switch
/// that stops the metadata call writing one: `--locked` and `--frozen` never
/// reach it, because rust-analyzer hardcodes `locked: false` for exactly
/// that invocation. The honest alternative — dropping the bit so the gate
/// prompts on every code-intelligence call — is the cost `ToolMeta` records
/// and refuses for the web tools: a gate that fires constantly trains the
/// operator to click through it. The cost of keeping the bit, stated because
/// it is not nothing: a crate that omits its lockfile gains an untracked one
/// the first time these tools run against it.
///
/// What it does *not* cover, stated rather than implied: rust-analyzer keeps
/// a cache under its own data directory, outside the project and outside this
/// fingerprint. `read_only` asks whether a call can damage this machine, and a
/// cache in the server's own directory is not that.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_server_writes_nothing_but_the_cargo_lock() {
    let Some((sandbox, pool)) = fixture().await else {
        return;
    };

    // Half one: no lockfile. The one file the server may add is Cargo.lock,
    // and what lands there has to be a genuine lockfile for this crate — the
    // cargo metadata write the doc above argues for, not some other file
    // wearing the name.
    let before = support::fingerprint(sandbox.root());
    assert!(!before.is_empty(), "the fixture is empty");

    let client = pool.client(&sandbox.canonical()).await.expect("started");
    let _ = tokio::time::timeout(Duration::from_secs(180), client.wait_ready()).await;
    let _ = FindReferences::new(pool.clone())
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/lib.rs", "line": 4, "symbol": "Config" }),
        )
        .await;
    let _ = DocumentSymbols::new(pool)
        .invoke(&sandbox.ctx, json!({ "file_path": "src/lib.rs" }))
        .await;

    let lock = std::fs::read_to_string(sandbox.root().join("Cargo.lock"))
        .expect("the server materialized a lockfile, as measured on 0.3.3033");
    assert!(
        lock.contains("name = \"lsp-fixture\""),
        "Cargo.lock does not describe this crate: {lock}"
    );

    let after = support::fingerprint(sandbox.root());
    let mut unexpected: Vec<&String> = Vec::new();
    for (name, bytes) in &after {
        match before.iter().find(|(b, _)| b == name) {
            Some((_, original)) if *original != *bytes => unexpected.push(name),
            None if name.as_str() != "Cargo.lock" => unexpected.push(name),
            _ => {}
        }
    }
    assert!(
        unexpected.is_empty(),
        "rust-analyzer wrote into the project beyond Cargo.lock: {unexpected:?}"
    );

    // Half two: a valid lockfile already present. Measured on 0.3.3033 it is
    // left byte-identical; this half fails if a future server starts rewriting
    // current lockfiles. rust-analyzer builds the project model with cargo, so
    // a machine without cargo cannot run this half either — and says so.
    let locked = Sandbox::new();
    locked.write("Cargo.toml", CARGO_TOML);
    locked.write("src/lib.rs", LIB_RS);
    let generated = std::process::Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(locked.root())
        .output();
    if generated.is_ok_and(|o| o.status.success()) {
        let before = support::fingerprint(locked.root());
        let pool = Arc::new(Pool::new());
        let client = pool.client(&locked.canonical()).await.expect("started");
        let _ = tokio::time::timeout(Duration::from_secs(180), client.wait_ready()).await;
        let _ = Hover::new(pool)
            .invoke(
                &locked.ctx,
                json!({ "file_path": "src/lib.rs", "line": 4, "symbol": "Config" }),
            )
            .await;
        assert_eq!(
            before,
            support::fingerprint(locked.root()),
            "rust-analyzer changed a project that already had a valid lockfile"
        );
    } else {
        eprintln!("SKIPPED: lockfile-present half — cargo generate-lockfile did not succeed");
    }
}

/// The `Edit`-then-ask rhythm, which is the one that finds a stale document
/// sync. The server is told about the file, the file changes underneath, and
/// the next question must be answered about the new bytes.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_edited_between_calls_is_re_synced() {
    let Some((sandbox, pool)) = fixture().await else {
        return;
    };
    let tool = Hover::new(pool);

    let first = tool
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/lib.rs", "line": 4, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("no tool error");
    assert!(first.content.contains("Configuration for the thing"));

    // Rewrite the doc comment, exactly as an `Edit` would.
    sandbox.write(
        "src/lib.rs",
        &LIB_RS.replace("Configuration for the thing", "Rewritten between calls"),
    );

    let second = tool
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/lib.rs", "line": 4, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("no tool error");
    eprintln!("--- Hover after edit ---\n{}", second.content);
    assert!(
        second.content.contains("Rewritten between calls"),
        "the server answered from the version it opened before the edit: {}",
        second.content
    );
}
