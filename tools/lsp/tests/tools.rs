//! The seven tools, end to end, against a fake server.
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
use emma_tools_lsp::{
    Completion, Diagnostics, DocumentSymbols, FindReferences, GoToDefinition, Hover, Pool,
    SignatureHelp,
};
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
        out.content.starts_with("server: 0.0.0-fake (Rust)"),
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
async fn a_file_no_language_claims_is_refused_rather_than_guessed_at() {
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
    assert!(err.detail().contains("no language server wired"), "{err}");
    // The refusal lists what it *does* serve, so the model learns the rule
    // rather than only this verdict.
    assert!(err.detail().contains("rs, sh"), "{err}");
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
    // Scoped to the *label* line rather than to the whole result, and it has to
    // be: since the language table landed, every rust answer carries the proc
    // macro caveat, whose last clause is "references into macro-generated code
    // will be missed". A whole-result `!contains("references")` therefore went
    // red on a correct answer — a test failing on a sentence that exists to make
    // answers more honest. The claim was always about the noun `render::locations`
    // was handed, so that is what is asserted.
    let label = out
        .content
        .lines()
        .find(|l| l.contains(" in 1 file"))
        .unwrap_or_else(|| panic!("no count line in: {}", out.content));
    assert!(
        !label.contains("references"),
        "the answer is labelled with the wrong question: {label}"
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
/// What breaks in the real world if this fails: `Hover` used to be the only one
/// of the seven that assembled its own empty-result sentence, with the call to
/// `no_results_line` inline in `Hover::run` where replacing it with a plain
/// string read as a tidy-up. It is `render::hover` now, beside the rule it
/// obeys, and this test is what says the move kept the behaviour rather than
/// only the shape. A model told "no type information" about a
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

// region: The two tools that ask about a point
// ---------------------------------------------------------------------------
// The two tools that ask about a point
//
// `Completion` and `SignatureHelp` are the first tools to take `after` instead
// of `symbol`, and the difference is not cosmetic: `prepare_after` finds the
// text and then puts the cursor at its **end**, in UTF-16 units. Every other
// tool in this crate wants the position where a name *starts*, so the one line
// that adds the width is the only thing standing between "complete after the
// dot" and "complete at the start of the identifier before it" — two questions
// with different, plausible, well-formed answers.
//
// Nothing checked it. These do, and they check it in the units the wire uses
// rather than in characters, because a character count and a UTF-16 count agree
// on every fixture anybody writes by hand.
// ---------------------------------------------------------------------------

/// The file every point test asks about. Line numbers are load-bearing, so it
/// is written out with them in mind rather than reusing `SOURCE`.
///
///  1 `pub fn shape(config: Config) {`
///  2 `    config.`
///  6 `    take(1, 2)`
/// 10 `    config.name; config.name`   — the ambiguous one
/// 14 `    "🎈".len`                    — an astral character inside `after`
/// 15 `    // 🎈 config.`               — an astral character *before* the match
const POINTS: &str = "pub fn shape(config: Config) {
    config.
}

pub fn call() -> u8 {
    take(1, 2)
}

pub fn twice(config: Config) {
    config.name; config.name
}

pub fn wide() {
    \"🎈\".len
    // 🎈 config.
}
";

/// A sandbox holding [`POINTS`], a fake answering from a table, and the log of
/// everything the client sent — which is where the cursor position is read
/// back from, since it is an argument to the server and never appears in the
/// rendered result.
async fn point_fixture(
    indexing: Indexing,
    responses: &[(&str, Value)],
) -> (Sandbox, Arc<Pool>, Arc<std::sync::Mutex<Vec<Value>>>) {
    std::env::set_var(READY_TIMEOUT_ENV, "300");
    let sandbox = Sandbox::new();
    sandbox.write("src/points.rs", POINTS);
    sandbox.write("src/notes.md", "# not rust\n");
    let root = sandbox.canonical();

    let mut fake = Fake::new(indexing);
    for (method, result) in responses {
        fake = fake.answers(method, result.clone());
    }
    let sent = fake.sent.clone();
    let client = fake.start(&root).await;
    let pool = Arc::new(Pool::new());
    pool.adopt(&root, client).await;
    (sandbox, pool, sent)
}

/// The `(line, character)` of the last request of `method`, as the server was
/// actually told it.
///
/// Panicking rather than returning an `Option` on purpose: "the tool never
/// asked" is the failure this helper exists to make loud, and it is exactly the
/// shape that lets a position test pass without a position.
fn position_sent(sent: &Arc<std::sync::Mutex<Vec<Value>>>, method: &str) -> (u64, u64) {
    let messages = sent.lock().expect("sent");
    let message = messages
        .iter()
        .rev()
        .find(|m| m["method"] == method)
        .unwrap_or_else(|| panic!("no {method} reached the server; it was sent: {messages:#?}"));
    let position = &message["params"]["position"];
    (
        position["line"].as_u64().expect("a line was sent"),
        position["character"]
            .as_u64()
            .expect("a character was sent"),
    )
}

/// Both tools, on a well-formed call, rendering what they are for.
///
/// The two answers are in the same table, so a tool sending the *other*
/// method still gets a well-formed, plausible, wrong reply — the same negative
/// control `go_to_definition_answers_the_definition_question…` uses, and for
/// the same reason.
#[tokio::test]
async fn the_point_tools_render_what_they_are_for() {
    let (sandbox, pool, sent) = point_fixture(
        Indexing::Finishes,
        &[
            (
                "textDocument/completion",
                json!({
                    "isIncomplete": false,
                    "items": [
                        { "label": "name", "kind": 5, "detail": "String", "sortText": "aaa" },
                        { "label": "clone()", "kind": 2, "filterText": "clone",
                          "insertText": "clone", "sortText": "bbb" },
                    ],
                }),
            ),
            (
                "textDocument/signatureHelp",
                json!({
                    "activeSignature": 0,
                    "activeParameter": 1,
                    "signatures": [{
                        "label": "fn take(a: u8, b: u8) -> u8",
                        "parameters": [{ "label": "a: u8" }, { "label": "b: u8" }],
                    }],
                }),
            ),
        ],
    )
    .await;

    let out = Completion::new(pool.clone())
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/points.rs", "line": 2, "after": "config." }),
        )
        .await
        .expect("no fault")
        .expect("no tool error");
    assert!(out.content.contains("field: name"), "{}", out.content);
    assert!(out.content.contains("String"), "{}", out.content);
    // The insert text is shown only where it differs from the label, which is
    // the case that puts the wrong characters in a file if it is dropped.
    assert!(
        out.content.contains("[inserts \"clone\"]"),
        "{}",
        out.content
    );
    assert_eq!(out.display.as_deref(), Some("2 completions"));
    assert!(
        out.content.starts_with("server: 0.0.0-fake (Rust)"),
        "{}",
        out.content
    );
    // The question it asked, not a neighbouring one.
    position_sent(&sent, "textDocument/completion");

    let out = SignatureHelp::new(pool)
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/points.rs", "line": 6, "after": "take(1, " }),
        )
        .await
        .expect("no fault")
        .expect("no tool error");
    assert!(
        out.content.contains("> fn take(a: u8, b: u8) -> u8"),
        "the active signature is not marked: {}",
        out.content
    );
    assert!(
        out.content.contains("argument 2: b: u8"),
        "the argument being typed is not named: {}",
        out.content
    );
    position_sent(&sent, "textDocument/signatureHelp");
}

/// **The cursor lands at the END of `after`, which is the entire contract of
/// this argument shape.**
///
/// What breaks in the real world if this fails: `after: "config."` with a
/// cursor at the *start* of the match asks the server what can be typed where
/// `config` already is. rust-analyzer answers that question too — with every
/// name in scope — so the result is a full, well-ordered, confident list of the
/// wrong completions. Nothing in the rendered output says which column was
/// asked about, which is why this reads the request rather than the answer.
#[tokio::test]
async fn the_cursor_lands_at_the_end_of_after_and_not_at_its_start() {
    let (sandbox, pool, sent) = point_fixture(
        Indexing::Finishes,
        &[
            ("textDocument/completion", json!([])),
            ("textDocument/signatureHelp", json!(null)),
        ],
    )
    .await;

    // `    config.` — the match starts at column 4 and is seven units wide.
    Completion::new(pool.clone())
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/points.rs", "line": 2, "after": "config." }),
        )
        .await
        .expect("no fault")
        .expect("an empty list is not an error");
    assert_eq!(
        position_sent(&sent, "textDocument/completion"),
        (1, 11),
        "the cursor was not put past the end of `after` (start would be 4, and \
         a real server answers that question too)"
    );

    // `    take(1, 2)` — start 4, eight units wide, so the cursor sits where
    // the second argument is being typed.
    SignatureHelp::new(pool.clone())
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/points.rs", "line": 6, "after": "take(1, " }),
        )
        .await
        .expect("no fault")
        .expect("no answer is not an error");
    assert_eq!(
        position_sent(&sent, "textDocument/signatureHelp"),
        (5, 12),
        "the cursor is not inside the call past the comma"
    );

    // `occurrence` picks which match, and the end is still the end: the second
    // `config.` on line 10 starts at column 17.
    Completion::new(pool)
        .invoke(
            &sandbox.ctx,
            json!({ "file_path": "src/points.rs", "line": 10, "after": "config.", "occurrence": 2 }),
        )
        .await
        .expect("no fault")
        .expect("an empty list is not an error");
    assert_eq!(
        position_sent(&sent, "textDocument/completion"),
        (9, 24),
        "occurrence 2 did not select the second match, or the width was not added \
         to it"
    );
}

/// **The column is in UTF-16 units, on both sides of the match.**
///
/// What breaks in the real world if this fails: a file with an emoji in a
/// string or a comment — every file with a log line in it, eventually — gets a
/// cursor one unit short per astral character, and the server answers about the
/// character next door. This is the same class as `doc.rs`'s `🎈` tests and it
/// is a *different* line of code: `prepare_after` measures the argument's own
/// width rather than re-reading the line, so `doc`'s converter being right does
/// not make this right.
///
/// Two probes, because the two halves fail separately. `"🎈".` is an astral
/// character inside `after` (the width), and `    // 🎈 config.` is one before
/// the match (the start, which `doc::locate` computes).
#[tokio::test]
async fn the_cursor_counts_utf16_units_on_both_sides_of_the_match() {
    let (sandbox, pool, sent) = point_fixture(
        Indexing::Finishes,
        &[("textDocument/completion", json!([]))],
    )
    .await;
    let tool = Completion::new(pool);

    // Line 14 is `    "🎈".len`. `"🎈".` is four characters and **five** UTF-16
    // units, because the balloon is two, so the cursor is at 4 + 5 = 9. A
    // character count gives 8 and a byte count gives 11; both are confident
    // answers about a different column.
    tool.invoke(
        &sandbox.ctx,
        json!({ "file_path": "src/points.rs", "line": 14, "after": "\"🎈\"." }),
    )
    .await
    .expect("no fault")
    .expect("an empty list is not an error");
    assert_eq!(
        position_sent(&sent, "textDocument/completion"),
        (13, 9),
        "an astral character inside `after` was counted as one unit"
    );

    // Line 15 is `    // 🎈 config.`: the match starts at UTF-16 column 10 and
    // is seven wide.
    tool.invoke(
        &sandbox.ctx,
        json!({ "file_path": "src/points.rs", "line": 15, "after": "config." }),
    )
    .await
    .expect("no fault")
    .expect("an empty list is not an error");
    assert_eq!(
        position_sent(&sent, "textDocument/completion"),
        (14, 17),
        "an astral character before the match shifted the column"
    );
}

/// The refusals, for both tools, in the words a model has to be able to correct
/// from in one move.
///
/// `after` has its own empty-string refusal and it is not cosmetic: an empty
/// string matches at column 0 of every line, so it would be a confident answer
/// about the start of the line rather than about the point that was asked
/// about.
#[tokio::test]
async fn the_point_tool_refusals_say_enough_to_be_fixed() {
    let (sandbox, pool, _sent) = point_fixture(Indexing::Finishes, &[]).await;
    let completion = Completion::new(pool.clone());
    let signature = SignatureHelp::new(pool);

    for (name, tool) in [
        ("Completion", &completion as &dyn Tool),
        ("SignatureHelp", &signature as &dyn Tool),
    ] {
        let call = |args: Value| {
            let ctx = &sandbox.ctx;
            let tool = &tool;
            async move { tool.invoke(ctx, args).await.expect("no fault") }
        };

        // Empty `after`, which would otherwise match at column 0 of any line.
        let err = call(json!({ "file_path": "src/points.rs", "line": 2, "after": "" }))
            .await
            .expect_err("an empty `after` must be refused");
        assert_eq!(err.kind(), "bad_arguments", "{name}: {err}");
        assert!(err.detail().contains("is empty"), "{name}: {err}");
        assert!(
            err.detail().contains("immediately before"),
            "{name}: the refusal does not say what to pass instead: {err}"
        );

        // The 1-based conventions, both of them.
        let err = call(json!({ "file_path": "src/points.rs", "line": 0, "after": "config." }))
            .await
            .expect_err("line 0 must be refused");
        assert!(err.detail().contains("1-based"), "{name}: {err}");
        let err = call(
            json!({ "file_path": "src/points.rs", "line": 2, "after": "config.", "occurrence": 0 }),
        )
        .await
        .expect_err("occurrence 0 must be refused");
        assert!(err.detail().contains("1-based"), "{name}: {err}");

        // An unknown key is refused rather than dropped: a silently ignored
        // argument reads to the model as one that has no effect.
        let err = call(
            json!({ "file_path": "src/points.rs", "line": 2, "after": "config.", "symbol": "config" }),
        )
        .await
        .expect_err("`symbol` is the other shape's argument and must be refused");
        assert!(err.detail().contains("symbol"), "{name}: {err}");

        // Not on that line — the common failure after an `Edit` moved things.
        // The message quotes the line, so the next call can be right.
        let err = call(json!({ "file_path": "src/points.rs", "line": 3, "after": "config." }))
            .await
            .expect_err("`config.` is not on line 3");
        assert_eq!(err.kind(), "bad_arguments", "{name}: {err}");
        assert!(err.detail().contains("does not appear"), "{name}: {err}");

        // Ambiguous without `occurrence`: line 10 has two `config.`, and
        // picking one would be a coin flip presented as an answer.
        let err = call(json!({ "file_path": "src/points.rs", "line": 10, "after": "config." }))
            .await
            .expect_err("two matches on line 10 must be refused");
        assert!(err.detail().contains("2 times"), "{name}: {err}");
        assert!(err.detail().contains("occurrence"), "{name}: {err}");

        // Containment, before anything is read and before a server hears about
        // it.
        for path in ["../outside.rs", "src/../../outside.rs"] {
            let err = call(json!({ "file_path": path, "line": 2, "after": "config." }))
                .await
                .expect_err("a path outside the root must not be reachable");
            assert_eq!(err.kind(), "bad_arguments", "{name} {path}: {err}");
        }

        // A file this crate has no server for is refused rather than guessed
        // at, and the refusal says what to use instead.
        let err = call(json!({ "file_path": "src/notes.md", "line": 1, "after": "not" }))
            .await
            .expect_err("markdown has no language server here");
        assert_eq!(err.kind(), "tool_unavailable", "{name}: {err}");
        assert!(
            err.detail().contains("no language server wired"),
            "{name}: {err}"
        );
        assert!(err.detail().contains("Grep"), "{name}: {err}");
    }
}

// endregion: The two tools that ask about a point

// region: The two tools that ask about a whole file
// ---------------------------------------------------------------------------
// The two tools that ask about a whole file
//
// `DocumentSymbols` and `Diagnostics` take one argument and now share one
// validator. That is the point of the shared validator and also its risk: a
// mutation to it disarms both tools at once, silently, and nothing here noticed
// when the shared `deny_unknown` was deleted during this round's own mutation
// run. This is the guard that was missing.
// ---------------------------------------------------------------------------

/// The file-only refusals, asserted on both tools that use them.
///
/// What breaks in the real world if this fails: a silently dropped key reads to
/// the model as a parameter that had no effect, so it concludes the behaviour is
/// impossible rather than that it misspelled `file_path`. `validate_args` is
/// pure and runs before any process, which is why this needs no server.
#[test]
fn the_file_tools_refuse_an_unknown_key_and_a_missing_path() {
    let pool = Arc::new(Pool::new());
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(DocumentSymbols::new(pool.clone())),
        Arc::new(Diagnostics::new(pool.clone())),
    ];
    for tool in tools {
        let name = tool.name();
        let err = tool
            .validate_args(&json!({ "file_path": "src/config.rs", "flie": 1 }))
            .expect_err("an unknown key must be refused");
        assert!(err.detail().contains("flie"), "{name}: {err}");
        // And the refusal lists what *is* accepted, so the next call is right.
        assert!(err.detail().contains("file_path"), "{name}: {err}");

        let err = tool
            .validate_args(&json!({}))
            .expect_err("the one required argument must be required");
        assert!(err.detail().contains("file_path"), "{name}: {err}");

        // The positive control: the shape the tool actually takes passes, or
        // every assertion above would hold for a validator that refused
        // everything.
        tool.validate_args(&json!({ "file_path": "src/config.rs" }))
            .expect("the documented shape must be accepted");
    }
}

// endregion: The two tools that ask about a whole file
