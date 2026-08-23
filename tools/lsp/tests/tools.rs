//! The four tools, end to end, against a fake server.
//!
//! Everything asserted here is a property of this crate rather than of
//! rust-analyzer — containment, the language gate, the shape of a rendered
//! result, and the rule that an empty answer is only "none" when the index was
//! finished. Driving them through a fake means they are checked on every
//! machine, including the ones with no language server installed, which is where
//! a suite that needed one would simply stop running.

mod support;

use std::sync::Arc;

use emma_tool_api::Tool;
use emma_tools_lsp::client::READY_TIMEOUT_ENV;
use emma_tools_lsp::{DocumentSymbols, FindReferences, GoToDefinition, Hover, Pool};
use serde_json::{json, Value};
use support::{fingerprint, Fake, Indexing, Sandbox};

const SOURCE: &str = r#"pub struct Config {
    pub name: String,
}

impl Config {
    pub fn new(name: String) -> Config {
        Config { name }
    }
}

pub fn merge(a: Config, b: Config) -> Config {
    Config { name: a.name }
}
"#;

/// A sandbox with one Rust file, and a pool holding a fake server for it.
async fn fixture(indexing: Indexing, responses: &[(&str, Value)]) -> (Sandbox, Arc<Pool>) {
    std::env::set_var(READY_TIMEOUT_ENV, "300");
    let sandbox = Sandbox::new();
    sandbox.write("src/config.rs", SOURCE);
    sandbox.write("src/notes.md", "# not rust\n");
    let root = sandbox.canonical();

    let mut fake = Fake::new(indexing);
    for (method, result) in responses {
        fake = fake.answers(method, result.clone());
    }
    let client = fake.start(&root).await;
    let pool = Arc::new(Pool::new());
    pool.adopt(&root, client).await;
    (sandbox, pool)
}

fn location(root: &std::path::Path, relative: &str, line: u32, character: u32) -> Value {
    json!({
        "uri": emma_tools_lsp::doc::to_uri(&root.join(relative)),
        "range": { "start": { "line": line, "character": character } },
    })
}

/// The result a model actually reads: file, line, and the line's text, grouped,
/// with the server named above it. Twenty locations with their source lines is
/// useful; twenty bare positions are a second round of `Read` calls.
#[tokio::test]
async fn references_come_back_with_the_source_line_that_makes_them_useful() {
    let sandbox = Sandbox::new();
    sandbox.write("src/config.rs", SOURCE);
    let root = sandbox.canonical();
    let refs = json!([
        location(&root, "src/config.rs", 4, 5),
        location(&root, "src/config.rs", 5, 29),
        // Outside the root: rust-analyzer answers with these constantly.
        json!({ "uri": "file:///elsewhere/serde/lib.rs", "range": { "start": { "line": 1, "character": 0 } } }),
    ]);
    let client = Fake::new(Indexing::Finishes)
        .answers("textDocument/references", refs)
        .start(&root)
        .await;
    let pool = Arc::new(Pool::new());
    pool.adopt(&root, client).await;

    let tool = FindReferences::new(pool);
    let out = tool
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/config.rs", "line": 1, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("no tool error");

    assert!(
        out.content.contains("2 references in 1 file"),
        "{}",
        out.content
    );
    assert!(out.content.contains("src/config.rs"), "{}", out.content);
    // 1-based lines, and the source line itself.
    assert!(out.content.contains("5: impl Config {"), "{}", out.content);
    assert!(out.content.contains("6: pub fn new"), "{}", out.content);
    // Containment: the outside hit is counted, named, and never shown.
    assert!(out.content.contains("1 further"), "{}", out.content);
    assert!(
        !out.content.contains("elsewhere"),
        "leaked: {}",
        out.content
    );
    // The server is named in the result, not in the description.
    assert!(
        out.content.starts_with("server: rust-analyzer 0.0.0-fake"),
        "{}",
        out.content
    );
}

/// The rule, through the real tool path. A model reading this must not be able
/// to come away thinking there are no references.
#[tokio::test]
async fn an_empty_answer_from_an_unindexed_server_does_not_read_as_none() {
    let (sandbox, pool) = fixture(
        Indexing::NeverFinishes,
        &[("textDocument/references", json!([]))],
    )
    .await;
    let out = FindReferences::new(pool)
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/config.rs", "line": 1, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("emptiness is not an error");

    assert!(out.content.contains("still indexing"), "{}", out.content);
    assert!(out.content.contains("not an answer"), "{}", out.content);
    assert!(!out.content.contains("there are none"), "{}", out.content);
}

/// …and the mirror image, which is the half that keeps the rule from being
/// satisfied by hedging everything. A finished index is allowed to say none.
#[tokio::test]
async fn an_empty_answer_from_a_finished_index_is_an_answer() {
    let (sandbox, pool) = fixture(
        Indexing::Finishes,
        &[("textDocument/references", json!([]))],
    )
    .await;
    let out = FindReferences::new(pool)
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/config.rs", "line": 1, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("emptiness is not an error");
    assert!(out.content.contains("there are none"), "{}", out.content);
    assert!(!out.content.contains("not evidence"), "{}", out.content);
}

/// Containment, on the way in. A path outside the root must be refused before
/// anything is read and before any server hears about it.
#[tokio::test]
async fn a_path_outside_the_root_is_refused() {
    let (sandbox, pool) = fixture(Indexing::Finishes, &[]).await;
    let tool = FindReferences::new(pool);
    for path in ["../outside.rs", "src/../../outside.rs"] {
        let err = tool
            .invoke(
                &sandbox.ctx,
                json!({ "file_path": path, "line": 1, "symbol": "Config" }),
            )
            .await
            .expect("no fault")
            .expect_err(&format!("{path} must not be reachable"));
        assert_eq!(err.kind(), "bad_arguments", "{path}: {err}");
    }

    // And the positive control, because rejecting every absolute path is the
    // cheapest wrong way to pass the test above.
    let inside = sandbox.canonical().join("src/config.rs");
    let out = tool
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": inside.to_string_lossy(), "line": 1, "symbol": "Config" }),
        )
        .await
        .expect("no fault");
    assert!(
        out.is_ok(),
        "an absolute path inside the root must work: {out:?}"
    );
}

/// Never a quiet text search. A file this crate has no server for is refused,
/// and the refusal says what to use instead *and* what that costs.
#[tokio::test]
async fn a_file_that_is_not_rust_is_refused_rather_than_guessed_at() {
    let (sandbox, pool) = fixture(Indexing::Finishes, &[]).await;
    let err = Hover::new(pool)
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/notes.md", "line": 1, "symbol": "not" }),
        )
        .await
        .expect("no fault")
        .expect_err("markdown has no language server here");
    assert_eq!(err.kind(), "tool_unavailable");
    assert!(err.detail().contains("not a Rust file"), "{err}");
    assert!(err.detail().contains("Grep"), "{err}");
}

/// The arguments a model gets wrong, and the messages that let it correct in
/// one move rather than re-reading the file.
#[tokio::test]
async fn the_argument_refusals_say_enough_to_be_fixed() {
    let (sandbox, pool) = fixture(Indexing::Finishes, &[]).await;
    let tool = GoToDefinition::new(pool);
    let call = |args: Value| {
        let ctx = &sandbox.ctx;
        let tool = &tool;
        async move { tool.invoke(ctx, args).await.expect("no fault") }
    };

    // A symbol that is not on the line named — the common failure after an
    // `Edit` moved things. The message quotes the line.
    let err = call(json!({ "file_path": "src/config.rs", "line": 2, "symbol": "Config" }))
        .await
        .expect_err("not on line 2");
    assert_eq!(err.kind(), "bad_arguments");
    assert!(err.detail().contains("pub name: String"), "{err}");

    // Ambiguity is refused rather than guessed at. Line 11 of the fixture is
    // `pub fn merge(a: Config, b: Config) -> Config {`, where the three
    // `Config`s are a parameter type, another parameter type and a return type
    // — three genuinely different questions, and picking one would be a coin
    // flip presented as an answer.
    let err = call(json!({ "file_path": "src/config.rs", "line": 11, "symbol": "Config" }))
        .await
        .expect_err("ambiguous");
    assert!(err.detail().contains("3 times"), "{err}");
    assert!(err.detail().contains("occurrence"), "{err}");
    assert!(call(
        json!({ "file_path": "src/config.rs", "line": 11, "symbol": "Config", "occurrence": 2 })
    )
    .await
    .is_ok());

    // Off the end of the file, and the 1-based conventions.
    assert!(
        call(json!({ "file_path": "src/config.rs", "line": 900, "symbol": "Config" }))
            .await
            .is_err()
    );
    assert!(
        call(json!({ "file_path": "src/config.rs", "line": 0, "symbol": "Config" }))
            .await
            .is_err()
    );

    // An unknown key is refused rather than dropped: a silently ignored
    // parameter reads to the model as one that has no effect.
    let err =
        call(json!({ "file_path": "src/config.rs", "line": 1, "symbol": "Config", "colum": 3 }))
            .await
            .expect_err("typo");
    assert!(err.detail().contains("colum"), "{err}");
}

#[tokio::test]
async fn hover_and_document_symbols_render_what_they_are_for() {
    let (sandbox, pool) = fixture(
        Indexing::Finishes,
        &[
            (
                "textDocument/hover",
                json!({ "contents": { "kind": "markdown", "value": "```rust\npub struct Config\n```" } }),
            ),
            (
                "textDocument/documentSymbol",
                json!([{
                    "name": "Config", "kind": 23,
                    "range": { "start": { "line": 0, "character": 0 } },
                    "children": [{ "name": "new", "kind": 6, "detail": "fn(String) -> Config",
                                   "range": { "start": { "line": 5, "character": 4 } } }],
                }]),
            ),
        ],
    )
    .await;

    let hover = Hover::new(pool.clone())
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/config.rs", "line": 1, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("no error");
    assert!(
        hover.content.contains("pub struct Config"),
        "{}",
        hover.content
    );

    let symbols = DocumentSymbols::new(pool)
        .invoke(&sandbox.ctx, json!({ "file_path": "src/config.rs" }))
        .await
        .expect("no fault")
        .expect("no error");
    assert!(
        symbols.content.contains("struct: Config"),
        "{}",
        symbols.content
    );
    assert!(
        symbols.content.contains("  method: new"),
        "{}",
        symbols.content
    );
    assert!(symbols.content.contains("(line 6)"), "{}", symbols.content);
}

/// The same test `tools/fs` runs, for the same reason: `read_only` is what the
/// approval gate consults before deciding not to interrupt a human, so it has to
/// be a fact rather than a claim. These tools declare it while spawning a
/// process, which makes the check more load-bearing here than there, not less.
#[tokio::test]
async fn the_read_only_declaration_holds() {
    let (sandbox, pool) = fixture(
        Indexing::Finishes,
        &[
            ("textDocument/references", json!([])),
            ("textDocument/definition", json!(null)),
            ("textDocument/hover", json!({ "contents": "x" })),
            ("textDocument/documentSymbol", json!([])),
        ],
    )
    .await;
    let before = fingerprint(sandbox.root());
    assert!(!before.is_empty(), "the fixture is empty");

    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FindReferences::new(pool.clone())),
        Arc::new(GoToDefinition::new(pool.clone())),
        Arc::new(Hover::new(pool.clone())),
        Arc::new(DocumentSymbols::new(pool)),
    ];
    let provocations = [
        json!({ "file_path": "src/config.rs", "line": 1, "symbol": "Config" }),
        json!({ "file_path": "src/config.rs" }),
        json!({ "file_path": "src/notes.md", "line": 1, "symbol": "not" }),
        json!({ "file_path": "../escape.rs", "line": 1, "symbol": "x" }),
        json!({ "file_path": "does-not-exist.rs", "line": 1, "symbol": "x" }),
    ];

    let mut exercised = 0;
    for tool in &tools {
        assert!(tool.meta().read_only, "{} stopped claiming it", tool.name());
        assert!(tool.meta().idempotent, "{}", tool.name());
        assert!(!tool.meta().reaches_network, "{}", tool.name());
        for args in &provocations {
            // The result is ignored on purpose: a tool that writes and *then*
            // errors is exactly what this catches.
            let _ = tool.invoke(&sandbox.ctx, args.clone()).await;
            exercised += 1;
        }
    }
    assert!(exercised >= 20, "only {exercised} calls");
    assert_eq!(
        before,
        fingerprint(sandbox.root()),
        "a tool declaring read_only modified the tree"
    );
}

// region: The gaps this file had
// ---------------------------------------------------------------------------
// The gaps this file had
//
// Everything above exercises `FindReferences` hard, `Hover` and
// `DocumentSymbols` once each on their happy path, and `GoToDefinition` only
// for the arguments it refuses. Two consequences, each of which survived a
// green suite until these were written:
//
//   * no test read what `GoToDefinition` *returns*, so the LSP method it sends
//     and the noun it labels the answer with were both unasserted;
//   * `GoToDefinition`, `Hover` and `DocumentSymbols` each pass `readiness`
//     into the renderer separately, and only `FindReferences`' path was
//     checked — the rule the whole crate is built around was one line of
//     copy-paste away from being silently dropped in three places.
//
// Each test below pairs the unready case with the ready one, because a test
// that only asserts the hedge is satisfied by hedging everything, which is the
// other way to get this wrong.
// ---------------------------------------------------------------------------

/// `GoToDefinition` must ask the *definition* question, and label the answer as
/// one.
///
/// What breaks in the real world if this fails: the tool sends some other LSP
/// method — `textDocument/references` is a few characters' worth of edit away
/// and is answered by the same server with the same shape — and a model asking
/// "where is this defined" is handed the list of call sites, labelled as
/// definitions, with nothing in the output saying which question was answered.
/// That is the crate's second rule ("never quietly answer a different
/// question") failing in the one place nothing else would notice.
#[tokio::test]
async fn go_to_definition_answers_the_definition_question_and_not_a_neighbouring_one() {
    let sandbox = Sandbox::new();
    sandbox.write("src/config.rs", SOURCE);
    let root = sandbox.canonical();

    // The negative control, and the point of the test: this server answers
    // *both* methods, with different lines. A tool that sends the wrong one
    // still gets a well-formed, plausible, wrong answer — which is what
    // shipping this defect would actually look like.
    let client = Fake::new(Indexing::Finishes)
        // A bare `Location` object rather than an array: one of the three
        // shapes `textDocument/definition` may reply with, and the one a
        // parser that handles only arrays renders as "there is no definition".
        .answers(
            "textDocument/definition",
            location(&root, "src/config.rs", 0, 11),
        )
        .answers(
            "textDocument/references",
            json!([location(&root, "src/config.rs", 10, 7)]),
        )
        .start(&root)
        .await;
    let pool = Arc::new(Pool::new());
    pool.adopt(&root, client).await;

    let out = GoToDefinition::new(pool)
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/config.rs", "line": 7, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("no tool error");

    assert!(
        out.content.contains("1: pub struct Config"),
        "the declaration line is what the caller asked for: {}",
        out.content
    );
    // The noun is a separate argument to `render::locations`, so the label and
    // the method are two independent things to get wrong.
    assert!(
        out.content.contains("1 definitions in 1 file"),
        "the answer is not labelled as definitions: {}",
        out.content
    );
    // The reference answer is reachable from this same server and must not be
    // what came back.
    assert!(
        !out.content.contains("merge"),
        "GoToDefinition returned the reference answer: {}",
        out.content
    );
    assert!(
        !out.content.contains("references"),
        "the answer is labelled with the wrong question: {}",
        out.content
    );
}

/// `GoToDefinition` with nothing to report must not say "there is none" unless
/// the index was finished.
///
/// What breaks in the real world if this fails: rust-analyzer answers
/// `definition` with `null` for the first ten to sixty seconds of a cold start.
/// A model told "no definitions found — there are none" about a symbol it is
/// about to edit concludes the symbol is dead. `FindReferences` is the only
/// tool whose readiness plumbing any test read; this is the same line of code
/// in a different function.
#[tokio::test]
async fn go_to_definition_with_nothing_to_report_respects_the_readiness_rule() {
    let (sandbox, pool) = fixture(
        Indexing::NeverFinishes,
        &[("textDocument/definition", Value::Null)],
    )
    .await;
    let unready = GoToDefinition::new(pool)
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/config.rs", "line": 1, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("emptiness is not an error");
    assert!(
        unready.content.contains("still indexing"),
        "{}",
        unready.content
    );
    assert!(
        unready.content.contains("not an answer"),
        "{}",
        unready.content
    );
    assert!(
        !unready.content.contains("there are none"),
        "an unindexed server was allowed to declare a symbol undefined: {}",
        unready.content
    );

    // The negative control. Hedging every answer would satisfy the half above
    // and destroy the tool.
    let (sandbox, pool) = fixture(
        Indexing::Finishes,
        &[("textDocument/definition", Value::Null)],
    )
    .await;
    let ready = GoToDefinition::new(pool)
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/config.rs", "line": 1, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("emptiness is not an error");
    assert!(
        ready.content.contains("No definitions found"),
        "{}",
        ready.content
    );
    assert!(
        ready.content.contains("there are none"),
        "a finished index must be allowed to answer: {}",
        ready.content
    );
    assert!(
        !ready.content.contains("still indexing"),
        "{}",
        ready.content
    );
}

/// An empty symbol list from an unfinished index must not read as "this file is
/// empty".
///
/// What breaks in the real world if this fails: `DocumentSymbols` is the tool
/// to reach for on the *first* look at an unfamiliar file, which is precisely
/// when the server is coldest. "No symbols found — there are none" about a
/// 900-line module is the most confidently wrong sentence this crate can emit,
/// and `render::symbols` takes `readiness` as its own argument, so nothing but
/// this test connects the two.
#[tokio::test]
async fn document_symbols_with_nothing_to_report_respects_the_readiness_rule() {
    let (sandbox, pool) = fixture(
        Indexing::NeverFinishes,
        &[("textDocument/documentSymbol", json!([]))],
    )
    .await;
    let unready = DocumentSymbols::new(pool)
        .invoke(&sandbox.ctx, json!({ "file_path": "src/config.rs" }))
        .await
        .expect("no fault")
        .expect("emptiness is not an error");
    assert!(
        unready.content.contains("still indexing"),
        "{}",
        unready.content
    );
    assert!(
        unready.content.contains("not an answer"),
        "{}",
        unready.content
    );
    assert!(
        !unready.content.contains("there are none"),
        "an unindexed server was allowed to call a Rust file symbol-less: {}",
        unready.content
    );

    // The negative control.
    let (sandbox, pool) = fixture(
        Indexing::Finishes,
        &[("textDocument/documentSymbol", json!([]))],
    )
    .await;
    let ready = DocumentSymbols::new(pool)
        .invoke(&sandbox.ctx, json!({ "file_path": "src/config.rs" }))
        .await
        .expect("no fault")
        .expect("emptiness is not an error");
    assert!(
        ready.content.contains("No symbols found"),
        "{}",
        ready.content
    );
    assert!(
        ready.content.contains("there are none"),
        "a finished index must be allowed to answer: {}",
        ready.content
    );
}

/// `Hover` with no type information must say which of the two things happened.
///
/// What breaks in the real world if this fails: `Hover` is the only one of the
/// four that assembles its own empty-result sentence rather than letting
/// `render::locations` or `render::symbols` do it — the call to
/// `no_results_line` sits inline in `Hover::run`, where replacing it with a
/// plain string reads as a tidy-up. A model told "no type information" about a
/// symbol on a cold server concludes the symbol has no type, and the usual next
/// move is to rewrite the code around it.
#[tokio::test]
async fn hover_with_no_type_information_respects_the_readiness_rule() {
    // `contents` absent entirely, which is what a server that has not yet
    // indexed the file replies with.
    let (sandbox, pool) = fixture(
        Indexing::NeverFinishes,
        &[("textDocument/hover", Value::Null)],
    )
    .await;
    let unready = Hover::new(pool)
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/config.rs", "line": 1, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("emptiness is not an error");
    assert!(
        unready.content.contains("still indexing"),
        "{}",
        unready.content
    );
    assert!(
        unready.content.contains("not an answer"),
        "{}",
        unready.content
    );
    assert!(
        !unready.content.contains("there are none"),
        "an unindexed server was allowed to declare a symbol untyped: {}",
        unready.content
    );

    // The negative control, and the half that also pins the noun: what the
    // model is told is missing is "type information", not "results".
    let (sandbox, pool) = fixture(Indexing::Finishes, &[("textDocument/hover", Value::Null)]).await;
    let ready = Hover::new(pool)
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/config.rs", "line": 1, "symbol": "Config" }),
        )
        .await
        .expect("no fault")
        .expect("emptiness is not an error");
    assert!(
        ready.content.contains("No type information found"),
        "{}",
        ready.content
    );
    assert!(
        ready.content.contains("there are none"),
        "a finished index must be allowed to answer: {}",
        ready.content
    );
}

// endregion: The gaps this file had
