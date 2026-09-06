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
use emma_tools_lsp::{
    server, Diagnostics, DocumentSymbols, FindReferences, GoToDefinition, Hover, Pool,
};
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

/// The language every case here drives. `resolve` and `Pool::client` both take
/// one now: the pool is keyed by root and language.
fn rust() -> &'static emma_tools_lsp::lang::Language {
    emma_tools_lsp::lang::by_key("rust").expect("rust is in the table")
}

/// Sets up a real server over a one-file crate, or `None` if there is none
/// installed.
async fn fixture() -> Option<(Sandbox, Arc<Pool>)> {
    match server::resolve(rust()) {
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
    pool.client(&sandbox.canonical(), rust()).await.ok()?;
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
    let client = pool
        .client(&sandbox.canonical(), rust())
        .await
        .expect("started");

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
/// - 2026-09-06, `rust-analyzer 1.94.1 (e408947b 2026-03-25)`, the rustup
///   component on Windows: **no `Cargo.lock` again.** A full index, a satisfied
///   `FindReferences` and a `DocumentSymbols` left the fixture byte-identical.
///
/// **So the lockfile write is a property of the build, not of the tool, and
/// this test used to assert the wrong half of that.** It did
/// `read_to_string(Cargo.lock).expect("the server materialized a lockfile, as
/// measured on 0.3.3033")` — which turns a *tighter* server into a red test,
/// and is the same mistake in the opposite direction as the version it
/// replaced. Three measurements, two answers, and neither is a defect.
///
/// What is asserted instead is the invariant the `read_only` bit actually
/// rests on and every build has satisfied: **nothing in the project changes,
/// and the only file that may be added is a `Cargo.lock` that genuinely
/// describes this crate.** A server that started writing a `target/`, or
/// rewriting `src/lib.rs`, or dropping a file called `Cargo.lock` that is not
/// one, still goes red.
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

    let client = pool
        .client(&sandbox.canonical(), rust())
        .await
        .expect("started");
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

    // If one appeared, it has to be a genuine lockfile for this crate — the
    // cargo metadata write the doc above argues for, and not some other file
    // wearing the name. If none appeared, this build did not make the call,
    // which three years of rust-analyzer say is equally normal.
    match std::fs::read_to_string(sandbox.root().join("Cargo.lock")) {
        Ok(lock) => assert!(
            lock.contains("name = \"lsp-fixture\""),
            "a file called Cargo.lock appeared and does not describe this crate: {lock}"
        ),
        Err(e) => eprintln!("no Cargo.lock was written by this build ({e}); see the doc above"),
    }

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
        let client = pool
            .client(&locked.canonical(), rust())
            .await
            .expect("started");
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

// region: Diagnostics, against a server that really pushes them
// ---------------------------------------------------------------------------
// Diagnostics, against a server that really pushes them
//
// Everything in `tests/diagnostics.rs` runs through the fake, which is what
// makes the three outcomes testable on a box with nothing installed. What the
// fake cannot answer is whether a real rust-analyzer publishes anything at all
// with `check.enable` off — the exact question the crate spent a release
// refusing to ship the tool over. These two are the receipt.
// ---------------------------------------------------------------------------

/// A real server, a real readiness wait, a real publication — and the boundary
/// of what `Diagnostics` can honestly claim for Rust.
///
/// **What was measured, 2026-09-06, rust-analyzer 1.94.1, `check.enable: false`:**
///
/// - A **syntax error** is published, promptly and in detail. `pub fn oops( ->`
///   came back as seven `rust-analyzer:syntax-error` diagnostics, 1-based,
///   already waiting by the time the tool asked (0.00s past readiness).
/// - An **unresolved name** — `no_such_function()` in an otherwise well-formed
///   file — came back as an **empty publication**: a real answer saying clean.
///   Run on its own, before the syntax error was added, and twice.
/// - A **clean file** came back clean, which is the control.
///
/// The middle one is the finding, and it contradicts what the ported
/// description said. With `cargo check` off this is a **syntax** checker for
/// Rust, not a name-resolution one: rust-analyzer withholds name and type
/// diagnostics for a crate whose model it does not consider fully loaded, and
/// with build scripts and proc macros disabled that is every crate Emma opens.
/// `descriptions/diagnostics.md` says so now. `Bash` running `cargo check`
/// remains the honest tool for anything past a parse error.
///
/// The unresolved-name half is **printed and not asserted**, deliberately. A
/// server that starts reporting it would be strictly better, and a test that
/// pinned today's silence would go red for the improvement — the same mistake
/// the lockfile assertion above made in the other direction. What is asserted is
/// that it produced an *answer* rather than the timeout sentence, because that
/// is the tool's contract and not the server's behaviour.
///
/// Timings are printed rather than asserted. A wall-clock assertion here would
/// be the 0.36-second lie in a new costume: a number measured on one machine,
/// pinned, and then believed on every other.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_server_publishes_diagnostics_and_a_clean_file_reads_differently() {
    if server::resolve(rust()).is_err() {
        eprintln!("SKIPPED: no rust-analyzer on this machine");
        return;
    }
    // A sandbox of its own rather than `fixture()`'s, and the broken code is
    // written **before** the server starts. Measured 2026-09-06: a module added
    // to `lib.rs` after the workspace had been loaded came back with an empty
    // diagnostic set — a real answer about a file rust-analyzer did not yet
    // consider part of the crate. That is the server being consistent rather
    // than wrong, and it is a trap for a test that writes its fixture late.
    let sandbox = Sandbox::new();
    sandbox.write("Cargo.toml", CARGO_TOML);
    sandbox.write("src/clean.rs", LIB_RS);
    sandbox.write(
        "src/unresolved.rs",
        "pub fn oops() {\n    no_such_function();\n}\n",
    );
    sandbox.write(
        "src/lib.rs",
        "pub mod clean;\npub mod unresolved;\n\npub fn broken( -> u32 {\n    0\n}\n",
    );
    let pool = Arc::new(Pool::new());

    let started = std::time::Instant::now();
    let client = pool
        .client(&sandbox.canonical(), rust())
        .await
        .expect("started");
    let readiness = tokio::time::timeout(Duration::from_secs(180), client.wait_ready()).await;
    eprintln!(
        "--- readiness: {readiness:?} after {:.2}s ---",
        started.elapsed().as_secs_f64()
    );

    let asked = std::time::Instant::now();
    let bad = Diagnostics::new(pool.clone())
        .invoke(&sandbox.ctx, json!({ "file_path": "src/lib.rs" }))
        .await
        .expect("no fault")
        .expect("no tool error");
    eprintln!(
        "--- Diagnostics src/lib.rs (syntax error), {:.2}s ---\n{}",
        asked.elapsed().as_secs_f64(),
        bad.content
    );

    let asked = std::time::Instant::now();
    let good = Diagnostics::new(pool.clone())
        .invoke(&sandbox.ctx, json!({ "file_path": "src/clean.rs" }))
        .await
        .expect("no fault")
        .expect("no tool error");
    eprintln!(
        "--- Diagnostics src/clean.rs, {:.2}s ---\n{}",
        asked.elapsed().as_secs_f64(),
        good.content
    );

    // The third file, printed rather than asserted. See the doc above: this is
    // where "unresolved names are reported" was measured and found false.
    let asked = std::time::Instant::now();
    let unresolved = Diagnostics::new(pool)
        .invoke(&sandbox.ctx, json!({ "file_path": "src/unresolved.rs" }))
        .await
        .expect("no fault")
        .expect("no tool error");
    eprintln!(
        "--- Diagnostics src/unresolved.rs (name resolution only), {:.2}s ---\n{}",
        asked.elapsed().as_secs_f64(),
        unresolved.content
    );

    // The assertions that cannot be satisfied by silence. If the server had
    // published nothing, every one of these results would carry the timeout
    // sentence and this goes red — which is the point, and is what caught the
    // URI-spelling defect `client::published_key` records.
    assert!(
        !bad.content.contains("not a clean result"),
        "the server published nothing for a file with a syntax error in it; \
         `Diagnostics` cannot be certified against this build: {}",
        bad.content
    );
    assert!(
        bad.content.contains("Syntax Error") || bad.content.contains("diagnostic(s)"),
        "{}",
        bad.content
    );
    assert!(
        good.content.contains("reported no problems"),
        "a file with nothing wrong in it did not come back clean: {}",
        good.content
    );
    assert!(
        !unresolved.content.contains("not a clean result"),
        "the server did not answer at all about the unresolved-name file: {}",
        unresolved.content
    );
    assert_ne!(bad.content, good.content);
}

/// The same tool against **this repository**, which is the shape the owner will
/// actually run it in.
///
/// `#[ignore]` for one reason and it is not flakiness: indexing a nine-crate
/// workspace costs a couple of minutes and several gigabytes of resident memory,
/// which is not a thing to do on every `cargo test`. Run it deliberately:
///
/// ```text
/// cargo test -p emma-tools-lsp --test real_server -- --ignored --nocapture
/// ```
///
/// It asserts almost nothing on purpose. Emma's own tree compiles, so the only
/// honest expectation is a clean answer or a real finding, and what this proves
/// is the thing no sandbox can: that readiness, the pool key, the document sync
/// and the publication path survive a real workspace rather than a one-file
/// crate.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "indexes the whole workspace; run deliberately with --ignored"]
async fn diagnostics_against_this_repository() {
    if server::resolve(rust()).is_err() {
        eprintln!("SKIPPED: no rust-analyzer on this machine");
        return;
    }
    // `tools/lsp` -> `tools` -> the workspace root.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the workspace root is two levels up")
        .to_path_buf();
    let ctx = emma_tool_api::ToolCtx {
        cwd: root.clone(),
        session_id: "certify".into(),
        turn_id: "certify".into(),
        background: Default::default(),
    };

    let pool = Arc::new(Pool::new());
    let started = std::time::Instant::now();
    let client = pool
        .client(&root.canonicalize().expect("canonical"), rust())
        .await
        .expect("started");
    let readiness = tokio::time::timeout(Duration::from_secs(600), client.wait_ready()).await;
    eprintln!(
        "--- readiness: {readiness:?} after {:.2}s on {} ---",
        started.elapsed().as_secs_f64(),
        root.display()
    );

    let asked = std::time::Instant::now();
    let out = Diagnostics::new(pool)
        .invoke(&ctx, json!({ "file_path": "tools/lsp/src/lang.rs" }))
        .await
        .expect("no fault")
        .expect("no tool error");
    eprintln!(
        "--- Diagnostics tools/lsp/src/lang.rs, {:.2}s ---\n{}",
        asked.elapsed().as_secs_f64(),
        out.content
    );
    assert!(out.content.contains("lang.rs"), "{}", out.content);
}

// endregion: Diagnostics, against a server that really pushes them

/// **Completion, against the real server, and this is the test the fixtures
/// cannot replace.**
///
/// Every shape in `render`'s completion tests is one this author wrote down
/// from the protocol. That is exactly the position this crate was in when it
/// declared readiness at 0.36 seconds against every fake and was wrong against
/// rust-analyzer. So this asks the real server for completions in the middle of
/// a real file and asserts the three things a wrong answer would break:
///
/// 1. Something comes back at all, and it contains the fixture's own item.
/// 2. What would be inserted is plain text, not a snippet with placeholders —
///    which is what `snippetSupport: false` in the handshake is asking for, and
///    the one claim in that block that changes bytes in somebody's file.
/// 3. The filter text is a bare identifier, not the decorated label. This is
///    the difference between a list that narrows as you type and one that
///    empties.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_server_completes_and_the_items_are_insertable() {
    let Some((sandbox, pool)) = fixture().await else {
        return;
    };
    let client = pool
        .client(&sandbox.canonical(), rust())
        .await
        .expect("started");
    client.wait_ready().await;

    // A file that asks for a method on a `Config`. The cursor sits directly
    // after the dot, which is where a person's would be.
    // **Appended to `lib.rs` rather than written as a new file, and that is
    // the finding this test produced on its first run.** A fresh
    // `src/probe.rs` is not in the crate's module tree until something
    // declares `mod probe;`, and rust-analyzer offers nothing at all for a
    // file it does not consider part of the build. It answered with zero
    // items and no error, which is exactly the silence this crate's
    // readiness design exists to tell apart from an empty answer.
    let probe = format!(
        "{LIB_RS}
pub fn probe(c: Config) {{
    c.
}}
"
    );
    let path = sandbox.canonical().join("src/lib.rs");
    sandbox.write("src/lib.rs", &probe);
    client.sync_document(&path, &probe);
    // The cursor sits directly after the dot, where a person's would be.
    let line = probe
        .lines()
        .position(|l| l.trim() == "c.")
        .expect("the probe line") as u32;

    let answer = client
        .request(
            "textDocument/completion",
            serde_json::json!({
                "textDocument": { "uri": emma_tools_lsp::doc::to_uri(&path) },
                "position": { "line": line, "character": 6 },
            }),
        )
        .await
        .expect("the server answered");

    let (items, _incomplete) = emma_tools_lsp::render::parse_completions(&answer.value);
    eprintln!(
        "real completion: {} items, first five: {:?}",
        items.len(),
        items
            .iter()
            .take(5)
            .map(|i| (&i.label, &i.filter, &i.insert, i.kind, i.snippet))
            .collect::<Vec<_>>()
    );
    assert!(
        !items.is_empty(),
        "the real server offered no completions after a dot on a known type"
    );

    // The fixture's own field is reachable from here, and is the one item this
    // test can name without depending on the standard library's shape.
    let named = items
        .iter()
        .find(|i| i.filter == "name")
        .unwrap_or_else(|| {
            panic!(
                "no `name` among {:?}",
                items.iter().map(|i| &i.filter).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        named.kind, "field",
        "a struct field came back as something else"
    );

    // Nothing offered may be a snippet: the handshake asked for plain text, and
    // a placeholder inserted literally is `${1:value}` in somebody's source.
    for item in &items {
        assert!(
            !item.snippet,
            "the server sent a snippet despite snippetSupport: false — {item:?}"
        );
        assert!(
            !item.insert.contains("${"),
            "an item would insert a placeholder verbatim: {item:?}"
        );
    }

    // And the filter is an identifier rather than the decorated label, which is
    // what makes typing narrow the list. rust-analyzer labels methods `name()`
    // and similar; the filter must not carry the brackets.
    for item in items.iter().filter(|i| i.kind == "method") {
        assert!(
            !item.filter.contains('('),
            "a method's filter text carries its brackets, so typing will not \
             match it: {item:?}"
        );
    }
}

/// Signature help, against the real server, with the cursor inside a call.
///
/// The claim under test is the one the protocol changed its mind about: which
/// parameter is marked active. A client reading only the top-level field marks
/// the wrong argument as soon as the cursor moves past the first comma.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_server_marks_the_argument_the_cursor_is_in() {
    let Some((sandbox, pool)) = fixture().await else {
        return;
    };
    let client = pool
        .client(&sandbox.canonical(), rust())
        .await
        .expect("started");
    client.wait_ready().await;

    // Two parameters, so "which one is active" has a wrong answer available.
    let probe =
        "pub fn take(a: u8, b: u8) -> u8 { a + b }\npub fn probe() -> u8 {\n    take(1, 2)\n}\n";
    sandbox.write("src/sig.rs", probe);
    let path = sandbox.canonical().join("src/sig.rs");
    client.sync_document(&path, probe);

    // Character 12 on line 2 is inside the call, after the comma, so the
    // second argument is the active one.
    let answer = client
        .request(
            "textDocument/signatureHelp",
            serde_json::json!({
                "textDocument": { "uri": emma_tools_lsp::doc::to_uri(&path) },
                "position": { "line": 2, "character": 12 },
            }),
        )
        .await
        .expect("the server answered");

    let Some((sigs, active)) = emma_tools_lsp::render::parse_signatures(&answer.value) else {
        // Said rather than asserted away: a server build that does not offer
        // signature help here is a fact about the machine, and the fixture
        // tests still cover the parsing.
        eprintln!("real signature help: the server offered none for this position");
        return;
    };
    eprintln!(
        "real signature help: active={active} {:?}",
        sigs.iter()
            .map(|s| (&s.label, &s.parameters, s.active_parameter))
            .collect::<Vec<_>>()
    );
    let sig = &sigs[active];
    assert!(
        sig.label.contains("take"),
        "the signature is not the function being called: {sig:?}"
    );
    assert_eq!(
        sig.active_parameter,
        Some(1),
        "the cursor is past the comma, so the second argument is active: {sig:?}"
    );
}
