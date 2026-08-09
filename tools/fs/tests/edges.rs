//! The edges. Each of these is a place where the tool could have done
//! something plausible and wrong instead.

mod support;

use serde_json::json;
use support::Sandbox;

// ---------------------------------------------------------------- Edit

#[tokio::test]
async fn edit_refuses_an_ambiguous_match_and_names_the_count() {
    let sandbox = Sandbox::new();
    sandbox.write_file("dup.rs", "let x = 1;\nlet y = 2;\nlet x = 1;\n");
    sandbox.ok("Read", json!({ "file_path": "dup.rs" })).await;

    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "dup.rs", "old_string": "let x = 1;", "new_string": "let x = 9;" }),
        )
        .await;

    assert_eq!(error.kind(), "bad_arguments");
    assert!(
        error.detail().contains("occurs 2 times"),
        "the count is what makes this actionable: {error}"
    );
    assert_eq!(
        sandbox.read_file("dup.rs"),
        "let x = 1;\nlet y = 2;\nlet x = 1;\n",
        "the refused edit changed the file anyway"
    );
}

#[tokio::test]
async fn edit_replaces_all_only_when_asked() {
    let sandbox = Sandbox::new();
    sandbox.write_file("dup.rs", "a\nb\na\n");
    sandbox.ok("Read", json!({ "file_path": "dup.rs" })).await;

    let outcome = sandbox
        .ok(
            "Edit",
            json!({ "file_path": "dup.rs", "old_string": "a", "new_string": "z", "replace_all": true }),
        )
        .await;
    assert!(outcome.content.contains("2 replacements"), "{outcome:?}");
    assert_eq!(sandbox.read_file("dup.rs"), "z\nb\nz\n");
}

#[tokio::test]
async fn edit_with_a_unique_anchor_edits_exactly_that() {
    let sandbox = Sandbox::new();
    sandbox.write_file("one.rs", "alpha\nbeta\ngamma\n");
    sandbox.ok("Read", json!({ "file_path": "one.rs" })).await;

    sandbox
        .ok(
            "Edit",
            json!({ "file_path": "one.rs", "old_string": "beta", "new_string": "BETA" }),
        )
        .await;
    assert_eq!(sandbox.read_file("one.rs"), "alpha\nBETA\ngamma\n");
}

#[tokio::test]
async fn edit_reports_a_missing_anchor_as_an_argument_error() {
    let sandbox = Sandbox::new();
    sandbox.write_file("one.rs", "alpha\n");
    sandbox.ok("Read", json!({ "file_path": "one.rs" })).await;

    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "one.rs", "old_string": "omega", "new_string": "x" }),
        )
        .await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("not found"), "{error}");
}

#[tokio::test]
async fn edit_refuses_a_no_op_and_an_empty_anchor_without_touching_the_disk() {
    let sandbox = Sandbox::new();
    sandbox.write_file("one.rs", "alpha\n");

    // Both are caught by validate_args, which is why they need no file to
    // exist and must not require one.
    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "absent.rs", "old_string": "a", "new_string": "a" }),
        )
        .await;
    assert!(error.detail().contains("identical"), "{error}");

    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "absent.rs", "old_string": "", "new_string": "a" }),
        )
        .await;
    assert!(error.detail().contains("empty"), "{error}");
}

#[tokio::test]
async fn edit_refuses_a_file_that_was_never_read() {
    let sandbox = Sandbox::new();
    sandbox.write_file("unseen.rs", "alpha\n");
    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "unseen.rs", "old_string": "alpha", "new_string": "beta" }),
        )
        .await;
    assert!(error.detail().contains("has not been read"), "{error}");
    assert_eq!(sandbox.read_file("unseen.rs"), "alpha\n");
}

// --------------------------------------------------------------- Write

#[tokio::test]
async fn write_refuses_to_clobber_a_file_that_was_never_read() {
    let sandbox = Sandbox::new();
    sandbox.write_file("precious.txt", "months of work\n");

    let error = sandbox
        .err(
            "Write",
            json!({ "file_path": "precious.txt", "content": "gone\n" }),
        )
        .await;

    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("has not been read"), "{error}");
    assert_eq!(
        sandbox.read_file("precious.txt"),
        "months of work\n",
        "the refused write clobbered the file anyway"
    );
}

#[tokio::test]
async fn write_allows_a_clobber_after_a_read() {
    let sandbox = Sandbox::new();
    sandbox.write_file("known.txt", "before\n");
    sandbox
        .ok("Read", json!({ "file_path": "known.txt" }))
        .await;
    sandbox
        .ok(
            "Write",
            json!({ "file_path": "known.txt", "content": "after\n" }),
        )
        .await;
    assert_eq!(sandbox.read_file("known.txt"), "after\n");
}

#[tokio::test]
async fn write_creates_a_new_file_without_a_prior_read() {
    // The rule is about destroying content, not about ceremony. Requiring a
    // read of a file that does not exist would make the tool unusable and would
    // teach the model to Read-then-ignore, which defeats the actual check.
    let sandbox = Sandbox::new();
    sandbox
        .ok(
            "Write",
            json!({ "file_path": "new/deep/file.txt", "content": "fresh\n" }),
        )
        .await;
    assert_eq!(sandbox.read_file("new/deep/file.txt"), "fresh\n");
}

#[tokio::test]
async fn write_refuses_when_the_file_changed_after_the_read() {
    let sandbox = Sandbox::new();
    sandbox.write_file("racy.txt", "first\n");
    sandbox.ok("Read", json!({ "file_path": "racy.txt" })).await;

    // Something else edits it — another process, another agent, the user.
    sandbox.write_file("racy.txt", "someone else's work, longer than before\n");

    let error = sandbox
        .err(
            "Write",
            json!({ "file_path": "racy.txt", "content": "mine\n" }),
        )
        .await;
    assert!(error.detail().contains("changed on disk"), "{error}");
    assert!(sandbox.read_file("racy.txt").contains("someone else"));
}

#[tokio::test]
async fn a_truncated_read_is_not_a_licence_to_overwrite() {
    let sandbox = Sandbox::new();
    let big: String = (1..=3000).map(|n| format!("line {n}\n")).collect();
    sandbox.write_file("big.txt", &big);

    let outcome = sandbox.ok("Read", json!({ "file_path": "big.txt" })).await;
    assert!(outcome.truncated);

    let error = sandbox
        .err(
            "Write",
            json!({ "file_path": "big.txt", "content": "the first 2000 lines only\n" }),
        )
        .await;
    assert!(error.detail().contains("only read in part"), "{error}");
    assert_eq!(sandbox.read_file("big.txt").lines().count(), 3000);
}

// ---------------------------------------------------------------- Read

#[tokio::test]
async fn read_says_when_it_truncated() {
    let sandbox = Sandbox::new();
    let big: String = (1..=2500).map(|n| format!("line {n}\n")).collect();
    sandbox.write_file("big.txt", &big);

    let outcome = sandbox.ok("Read", json!({ "file_path": "big.txt" })).await;

    assert!(outcome.truncated, "the flag was not set");
    assert!(
        outcome.content.contains("[truncated"),
        "the model reads content, not the flag: {}",
        &outcome.content[outcome.content.len().saturating_sub(200)..]
    );
    assert!(
        outcome.content.contains("of 2500"),
        "the size it did not see must be stated"
    );
    assert!(!outcome.content.contains("line 2001\n"));
}

#[tokio::test]
async fn read_clips_a_very_long_line_and_says_so() {
    let sandbox = Sandbox::new();
    sandbox.write_file("long.txt", &format!("{}\nshort\n", "x".repeat(5000)));
    let outcome = sandbox.ok("Read", json!({ "file_path": "long.txt" })).await;
    assert!(outcome.truncated);
    assert!(outcome.content.contains("line clipped"), "{outcome:?}");
}

#[tokio::test]
async fn read_of_an_empty_file_succeeds() {
    let sandbox = Sandbox::new();
    sandbox.write_file("empty.txt", "");
    let outcome = sandbox
        .ok("Read", json!({ "file_path": "empty.txt" }))
        .await;
    assert_eq!(outcome.content, "");
    assert!(!outcome.truncated);
    assert_eq!(outcome.display.as_deref(), Some("empty.txt: empty file"));
}

#[tokio::test]
async fn read_numbers_lines_and_honours_offset_and_limit() {
    let sandbox = Sandbox::new();
    sandbox.write_file("n.txt", "one\ntwo\nthree\nfour\n");
    let outcome = sandbox
        .ok(
            "Read",
            json!({ "file_path": "n.txt", "offset": 2, "limit": 2 }),
        )
        .await;
    assert_eq!(outcome.content, "     2\ttwo\n     3\tthree\n\n[truncated: showing lines 2-3 of 4; continue with offset 4]\n");
    assert!(outcome.truncated);
}

#[tokio::test]
async fn read_of_a_missing_path_or_a_directory_is_an_argument_error() {
    let sandbox = Sandbox::new();
    sandbox.write_file("dir/inner.txt", "x");

    let error = sandbox
        .err("Read", json!({ "file_path": "nope.txt" }))
        .await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("does not exist"), "{error}");

    let error = sandbox.err("Read", json!({ "file_path": "dir" })).await;
    assert!(error.detail().contains("is a directory"), "{error}");
}

#[tokio::test]
async fn read_of_a_binary_file_fails_rather_than_returning_mojibake() {
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.root().join("blob.bin"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
    let error = sandbox
        .err("Read", json!({ "file_path": "blob.bin" }))
        .await;
    assert_eq!(error.kind(), "tool_failed");
    assert!(error.detail().contains("not valid UTF-8"), "{error}");
}

// ---------------------------------------------------------- Glob & Grep

#[tokio::test]
async fn glob_matching_nothing_succeeds_with_an_empty_list() {
    let sandbox = Sandbox::new();
    sandbox.write_file("a.rs", "fn main() {}\n");
    let outcome = sandbox.ok("Glob", json!({ "pattern": "**/*.ocaml" })).await;
    assert_eq!(outcome.content, "");
    assert!(!outcome.truncated);
}

#[tokio::test]
async fn glob_finds_files_and_skips_git() {
    let sandbox = Sandbox::new();
    sandbox.write_file("src/a.rs", "");
    sandbox.write_file("src/deep/b.rs", "");
    sandbox.write_file(".git/objects/c.rs", "");

    let outcome = sandbox.ok("Glob", json!({ "pattern": "**/*.rs" })).await;
    let mut found: Vec<&str> = outcome.content.lines().collect();
    found.sort_unstable();
    assert_eq!(found, ["src/a.rs", "src/deep/b.rs"]);
}

#[tokio::test]
async fn glob_rejects_a_malformed_pattern_before_walking() {
    let sandbox = Sandbox::new();
    let error = sandbox.err("Glob", json!({ "pattern": "a[" })).await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("not a valid glob"), "{error}");
}

#[tokio::test]
async fn grep_with_no_hits_succeeds() {
    let sandbox = Sandbox::new();
    sandbox.write_file("a.rs", "fn main() {}\n");
    let outcome = sandbox
        .ok("Grep", json!({ "pattern": "nowhere_at_all" }))
        .await;
    assert_eq!(outcome.content, "");
    assert!(!outcome.truncated);
    // The human-facing line has to distinguish "searched, found nothing" from
    // "produced nothing"; without this the empty-result branch can be deleted
    // and every assertion above still passes.
    assert_eq!(
        outcome.display.as_deref(),
        Some("nowhere_at_all: no matches in 1 files")
    );
}

#[tokio::test]
async fn grep_reports_path_line_and_text() {
    let sandbox = Sandbox::new();
    sandbox.write_file("a.rs", "one\nneedle here\nthree\n");
    sandbox.write_file("b.txt", "needle again\n");

    let outcome = sandbox.ok("Grep", json!({ "pattern": "needle" })).await;
    let mut lines: Vec<&str> = outcome.content.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, ["a.rs:2:needle here", "b.txt:1:needle again"]);

    let outcome = sandbox
        .ok(
            "Grep",
            json!({ "pattern": "needle", "glob": "**/*.rs", "output_mode": "count" }),
        )
        .await;
    assert_eq!(outcome.content, "a.rs:1");

    let outcome = sandbox
        .ok(
            "Grep",
            json!({ "pattern": "NEEDLE", "case_insensitive": true, "output_mode": "files_with_matches" }),
        )
        .await;
    assert_eq!(outcome.content.lines().count(), 2);
}

#[tokio::test]
async fn grep_skips_binary_files_instead_of_failing_the_search() {
    let sandbox = Sandbox::new();
    sandbox.write_file("a.rs", "needle\n");
    std::fs::write(sandbox.root().join("blob.bin"), [0xff, 0xfe, 0x00]).unwrap();
    let outcome = sandbox.ok("Grep", json!({ "pattern": "needle" })).await;
    assert_eq!(outcome.content, "a.rs:1:needle");
}

#[tokio::test]
async fn grep_rejects_a_malformed_regex_and_an_unknown_mode() {
    let sandbox = Sandbox::new();
    let error = sandbox.err("Grep", json!({ "pattern": "a(" })).await;
    assert!(error.detail().contains("not a valid regex"), "{error}");

    let error = sandbox
        .err("Grep", json!({ "pattern": "a", "output_mode": "sideways" }))
        .await;
    assert!(error.detail().contains("output_mode"), "{error}");
}

// ---------------------------------------------------------------- Bash

#[tokio::test]
async fn bash_runs_a_command_in_the_working_directory() {
    let sandbox = Sandbox::new();
    sandbox.write_file("marker.txt", "hi\n");
    let outcome = sandbox.ok("Bash", json!({ "command": "ls" })).await;
    assert!(outcome.content.contains("marker.txt"), "{outcome:?}");
}

#[tokio::test]
async fn bash_gets_a_shell_not_an_argv_exec() {
    // Pipes and `&&` are the reason this tool takes a command line rather than
    // an argv array; if that ever changes, this is the test that says so.
    let sandbox = Sandbox::new();
    let outcome = sandbox
        .ok(
            "Bash",
            json!({ "command": "printf 'a\\nb\\nc\\n' | grep b && echo joined" }),
        )
        .await;
    assert!(outcome.content.contains('b'));
    assert!(outcome.content.contains("joined"), "{outcome:?}");
}

#[tokio::test]
async fn bash_reports_a_non_zero_exit_with_its_output() {
    let sandbox = Sandbox::new();
    let outcome = sandbox
        .ok(
            "Bash",
            json!({ "command": "echo out; echo problem >&2; exit 3" }),
        )
        .await;
    // A command that ran and exited 3 answered the question. `grep -q` says
    // "no" with exit 1; `cargo test` says "three failed" with 101. Reporting
    // those as tool failures tells the model its shell is broken and invites it
    // to route around a problem it does not have.
    assert!(outcome.content.contains("exit status 3"), "{outcome:?}");
    assert!(
        outcome.content.contains("problem"),
        "the model needs what the command said, not just the code: {outcome:?}"
    );
    assert!(
        outcome.content.contains("out"),
        "stdout must survive a non-zero exit: {outcome:?}"
    );
}

#[tokio::test]
async fn bash_kills_a_command_that_outruns_its_timeout() {
    let sandbox = Sandbox::new();
    let started = std::time::Instant::now();
    let error = sandbox
        .err("Bash", json!({ "command": "sleep 30", "timeout_ms": 700 }))
        .await;
    assert!(error.detail().contains("killed after 700ms"), "{error}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "the timeout did not actually stop the wait"
    );
}

#[tokio::test]
async fn bash_caps_output_and_says_so() {
    let sandbox = Sandbox::new();
    let outcome = sandbox
        .ok(
            "Bash",
            json!({ "command": "i=0; while [ $i -lt 4000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done", "timeout_ms": 60000 }),
        )
        .await;
    assert!(outcome.truncated, "160 KB of output was not capped");
    assert!(
        outcome.content.contains("[truncated"),
        "{}",
        &outcome.content[..80]
    );
    assert!(
        outcome.content.len() < 200 * 1024,
        "the cap let {} bytes through",
        outcome.content.len()
    );
}

#[tokio::test]
async fn bash_does_not_hand_the_child_the_parent_environment() {
    let sandbox = Sandbox::new();
    std::env::set_var("EMMA_TEST_SECRET", "hunter2");
    let outcome = sandbox
        .ok(
            "Bash",
            json!({ "command": "echo \"[${EMMA_TEST_SECRET:-absent}]\"" }),
        )
        .await;
    std::env::remove_var("EMMA_TEST_SECRET");
    assert!(
        outcome.content.contains("[absent]"),
        "the allowlist leaked a parent variable: {outcome:?}"
    );
}

#[tokio::test]
async fn bash_starts_in_a_contained_subdirectory_when_asked() {
    let sandbox = Sandbox::new();
    sandbox.write_file("sub/inner.txt", "x");
    let outcome = sandbox
        .ok("Bash", json!({ "command": "ls", "cwd": "sub" }))
        .await;
    assert!(outcome.content.contains("inner.txt"), "{outcome:?}");
}

#[tokio::test]
async fn bash_refuses_an_empty_command() {
    let sandbox = Sandbox::new();
    let error = sandbox.err("Bash", json!({ "command": "   " })).await;
    assert_eq!(error.kind(), "bad_arguments");
}

// --------------------------------------------------------------- Shared

#[tokio::test]
async fn unknown_parameters_are_refused_rather_than_ignored() {
    let sandbox = Sandbox::new();
    sandbox.write_file("a.txt", "x\n");
    let error = sandbox
        .err("Read", json!({ "file_path": "a.txt", "recursive": true }))
        .await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("recursive"), "{error}");
}

#[tokio::test]
async fn a_wrongly_typed_argument_names_the_type_it_got() {
    let sandbox = Sandbox::new();
    let error = sandbox.err("Read", json!({ "file_path": 7 })).await;
    assert!(error.detail().contains("must be a string"), "{error}");

    let error = sandbox
        .err("Read", json!({ "file_path": "a.txt", "limit": "lots" }))
        .await;
    assert!(error.detail().contains("must be a number"), "{error}");
}

#[tokio::test]
async fn the_read_tracker_does_not_carry_across_sessions() {
    // A resumed process must not inherit a claim that some earlier session had
    // looked at a file; otherwise "read in this session" quietly becomes "read
    // at some point by somebody".
    let sandbox = Sandbox::new();
    sandbox.write_file("s.txt", "one\n");
    sandbox.ok("Read", json!({ "file_path": "s.txt" })).await;

    let other = emma_tool_api::ToolCtx {
        cwd: sandbox.root().to_path_buf(),
        session_id: "a-later-session".into(),
        turn_id: "t".into(),
    };
    let error = sandbox
        .tool("Write")
        .invoke(&other, json!({ "file_path": "s.txt", "content": "two\n" }))
        .await
        .expect("no fault")
        .expect_err("the later session should not inherit the read");
    assert!(error.detail().contains("has not been read"), "{error}");
}
