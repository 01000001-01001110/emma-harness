//! The test that turns `ToolMeta::read_only` from a declaration into a fact.
//!
//! In tustle-agent the equivalent field was declared and read by nothing. Here
//! the approval gate consults it, so the field being wrong is the difference
//! between a prompt appearing and a file being silently overwritten. This test
//! runs every `read_only` tool against a populated sandbox — with arguments
//! chosen to be as provocative as the schema allows — and asserts the tree is
//! byte-for-byte identical afterwards.

mod support;

use serde_json::json;
use support::{fingerprint, Sandbox};

/// Calls that a tool lying about `read_only` would most plausibly use to write.
fn provocations(name: &str) -> Vec<serde_json::Value> {
    match name {
        "Read" => vec![
            json!({ "file_path": "src/main.rs" }),
            json!({ "file_path": "empty.txt" }),
            json!({ "file_path": "does-not-exist.txt" }),
            json!({ "file_path": "nested/deep/notes.md", "offset": 2, "limit": 1 }),
            json!({ "file_path": "." }),
        ],
        "Glob" => vec![
            json!({ "pattern": "**/*" }),
            json!({ "pattern": "**/*.rs", "path": "src" }),
            json!({ "pattern": "nothing-matches-this" }),
        ],
        "Grep" => vec![
            json!({ "pattern": "fn" }),
            json!({ "pattern": "fn", "output_mode": "count" }),
            json!({ "pattern": "fn", "output_mode": "files_with_matches" }),
            json!({ "pattern": "no-such-string-anywhere" }),
            json!({ "pattern": "[", }),
        ],
        other => panic!("no provocations written for {other}"),
    }
}

#[tokio::test]
async fn tools_that_declare_read_only_change_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write_file("src/main.rs", "fn main() {}\n");
    sandbox.write_file("nested/deep/notes.md", "one\ntwo\nthree\n");
    sandbox.write_file("empty.txt", "");

    let before = fingerprint(sandbox.root());
    assert!(!before.is_empty(), "the sandbox fixture is empty");

    let mut exercised = 0;
    for tool in &sandbox.tools {
        if !tool.meta().read_only {
            continue;
        }
        for args in provocations(tool.name()) {
            // The result is deliberately ignored. Whether the call succeeded or
            // failed, it must not have written — a tool that errors *after*
            // touching the disk is exactly the case this catches.
            let _ = tool.invoke(&sandbox.ctx, args).await;
            exercised += 1;
        }
    }

    assert!(
        exercised >= 10,
        "only {exercised} read-only calls exercised"
    );
    assert_eq!(
        before,
        fingerprint(sandbox.root()),
        "a tool declaring read_only modified the tree"
    );
}

#[tokio::test]
async fn the_read_only_declarations_are_the_expected_ones() {
    // A pin, not a tautology: if someone flips `Bash` to read_only the test
    // above would still pass, because it only exercises the tools that claim
    // it. This is the half that notices a *new* claim.
    let sandbox = Sandbox::new();
    let mut claimed: Vec<&str> = sandbox
        .tools
        .iter()
        .filter(|t| t.meta().read_only)
        .map(|t| t.name())
        .collect();
    claimed.sort_unstable();
    assert_eq!(claimed, ["Glob", "Grep", "Read"]);
}

#[tokio::test]
async fn the_mutating_tools_actually_mutate() {
    // The mirror image, and the reason it is worth writing: `read_only: false`
    // on a tool that in fact cannot write would make the approval gate prompt
    // for nothing, and prompts nobody needs are how an operator learns to
    // approve without reading.
    let sandbox = Sandbox::new();
    let before = fingerprint(sandbox.root());

    sandbox
        .ok(
            "Write",
            json!({ "file_path": "made.txt", "content": "alpha\n" }),
        )
        .await;
    sandbox
        .ok(
            "Edit",
            json!({ "file_path": "made.txt", "old_string": "alpha", "new_string": "beta" }),
        )
        .await;

    assert_ne!(before, fingerprint(sandbox.root()));
    assert_eq!(sandbox.read_file("made.txt"), "beta\n");
}
