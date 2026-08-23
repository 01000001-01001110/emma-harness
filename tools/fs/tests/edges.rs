//! The edges. Each of these is a place where the tool could have done
//! something plausible and wrong instead.
//!
//! "Plausible and wrong" is the operative phrase, and it is why these are worth
//! more than their line count suggests. None of them catches a crash. They
//! catch the version of each tool that returns `Ok`, writes a file, and leaves
//! the model believing something that is not so: an edit that picked one of two
//! candidates, a write that discarded the half of the file nobody read, a read
//! that stopped at 2000 lines without saying so, a search that reported "not
//! found" because it could not look. Every one of those failures is invisible
//! at the call site and expensive several turns later.
//!
//! Each test below carries a line saying what breaks if it is deleted, because
//! a test whose purpose is only legible from its assertions is one a later
//! reader will weaken to make a change compile.

mod support;

use serde_json::json;
use support::Sandbox;

// region: Edit
// ---------------------------------------------------------------------------
// Edit
//
// Everything here defends the same property: an anchored change goes exactly
// where the model meant, or it does not happen. The refusals dominate, so the
// positive controls among them are the ones doing the unglamorous work.
// ---------------------------------------------------------------------------

/// Without this, `Edit` may quietly acquire a "first match wins" behaviour. It
/// would pass every other edit test in this file, and it would silently edit
/// code the model was not looking at — surfacing turns later as a test failure
/// in a file the transcript never mentions. The count in the message is
/// asserted too, because that is what tells the model to extend its anchor
/// rather than conclude the text is absent.
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

/// The other half of the pair above: the refusal must be escapable on purpose.
/// Delete this and the obvious way to make the ambiguity test pass is to make
/// `replace_all` a no-op, which turns "rename every occurrence" into a
/// permanently impossible request.
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

/// The positive control. Everything else here asserts a refusal, and a tool
/// that refused *everything* would satisfy all of them. This is the test that
/// notices the anchor matched the right line and left its neighbours alone.
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

/// An absent anchor is the model's mistake, not the machinery's, so it must
/// come back as `bad_arguments`. Classed as `tool_failed` instead it would read
/// to the loop as a broken tool — and a failed tool is not invoked twice in one
/// turn, so one mistyped `old_string` would cost the model `Edit` for the rest
/// of the turn.
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

/// Guards the split between `validate_args` and `invoke`. Both of these are
/// decidable from the arguments alone, and the test proves it by naming a file
/// that does not exist: if either check ever migrates into `invoke`, it starts
/// needing a filesystem and this goes red. The empty anchor matters
/// specifically because `"".matches()` would count once per character, so an
/// unguarded empty `old_string` matches at every position rather than nowhere,
/// so what the model would get is an insertion somewhere it never named — and
/// the message points it at `Write`, which is the tool it actually wanted.
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

/// `Edit` is anchored, which makes it tempting to treat as safe enough to skip
/// the read requirement. It is not: an anchor that happens to match inside a
/// file nobody looked at is a coincidence, not a location. Delete this and
/// `Edit` becomes the way around `Write`'s refusal.
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

// endregion: Edit

// region: Write
// ---------------------------------------------------------------------------
// Write
//
// The three ways a read can fail to license a whole-file overwrite — never
// read, changed since, read only in part — plus the two calls that must still
// go through, because a `Write` that refuses everything passes all three.
// ---------------------------------------------------------------------------

/// The central one. This is the failure the whole read-tracker exists for: an
/// agent asked to add a function satisfies the request by writing a file
/// containing exactly that function, the call returns `Ok`, and months of work
/// are gone with nothing in the transcript saying so. The second assertion is
/// the load-bearing half — a refusal that has already written the file is not a
/// refusal.
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

/// The positive control for the refusal above, and the reason it is not
/// optional: a `Write` wired to a tracker its `Read` does not share refuses
/// everything, which passes every refusal test in this file. This is the one
/// that notices. The same shape catches the tracker keying reads under a
/// spelling of the path that `Write` never looks up.
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

/// What breaks without it: someone tightens the rule to "always read first",
/// every new file becomes a two-call ritual beginning with a `Read` that
/// errors, and the model learns to treat `Read` as a formality to get past.
/// A safety check the model has been trained to satisfy without looking is
/// worse than none. The nested path also pins that parents are created.
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

/// "I read it, then something else wrote it, then I overwrote that" is the same
/// blind clobber wearing a receipt, and it is the one the operator hits in
/// practice — the file is open in their editor while the agent works. Delete
/// this and the tracker can be simplified to a set of paths, which passes the
/// never-read test and loses the user's edit.
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

/// The third refusal, and the least obvious of the three: the read succeeded,
/// so a tracker recording only "was it read" says yes. What it saw was 2000 of
/// 3000 lines. Without this the model can page a large file, write back the
/// window it happened to be shown, and delete the other third — and the call
/// looks exactly like a legitimate one. The final assertion checks the file,
/// not the error, because a refusal reported after the write is not a refusal.
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

// endregion: Write

// region: Read
// ---------------------------------------------------------------------------
// Read
//
// Bounded, and loud about it. Each cap has its own test because they fire
// independently, and the flag and the note in the content are asserted
// separately because a caller may render only one of them.
// ---------------------------------------------------------------------------

/// Silent truncation is the expensive kind of wrong: a truncated file and a
/// short file are indistinguishable to a model that was not told, and it will
/// then state confident things about a function it never saw. The flag alone is
/// not enough — the model reads content, not the struct — so both are asserted,
/// and so is the total, because "there is more" without "how much more" does
/// not tell it whether paging on is worth the turns.
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

/// A minified bundle or an embedded blob is one line and megabytes wide, so the
/// line cap has to exist independently of the line *count* cap. This is the
/// test that keeps the two separate: a clipped line still sets `truncated`,
/// which is what stops `Write` from accepting that read as a whole-file view.
#[tokio::test]
async fn read_clips_a_very_long_line_and_says_so() {
    let sandbox = Sandbox::new();
    sandbox.write_file("long.txt", &format!("{}\nshort\n", "x".repeat(5000)));
    let outcome = sandbox.ok("Read", json!({ "file_path": "long.txt" })).await;
    assert!(outcome.truncated);
    assert!(outcome.content.contains("line clipped"), "{outcome:?}");
}

/// Emptiness is a result. The tempting alternative is to return a helpful
/// "(file is empty)" in the content, and this pins that it does not: anything
/// written there is indistinguishable from file content the next time the model
/// quotes it back, and an `Edit` anchored on that phrase would never match. The
/// human-facing note goes in `display`, which is asserted here so the branch
/// cannot be deleted while every other assertion still passes.
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

/// The exact-bytes test, and deliberately brittle. Three things are pinned that
/// nothing else would catch: the numbers are absolute file lines rather than
/// window offsets — so line 2 reads `2` and not `1`, and an `Edit` anchor taken
/// from a paged read is not off by the offset — the continuation hint names the
/// next unread line, so paging forward neither skips nor repeats, and the label
/// is `<number>#<hash>` rather than a bare number, which is the string
/// `Edit.lines` parses. The hashes are written out literally because they are a
/// wire format: a change to the hash function or its width silently invalidates
/// every label in every transcript the model is still working from.
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
    assert_eq!(outcome.content, "     2#5778\ttwo\n     3#e204\tthree\n\n[truncated: showing lines 2-3 of 4, cut by limit=2; continue with offset 4]\n");
    assert!(outcome.truncated);
    // The same sentence, verbatim, on the field the runtime quotes to the model
    // and to the terminal. Two spellings of one cut is how the terminal came to
    // say a page was truncated while naming a limit that had not fired.
    assert_eq!(
        outcome.truncation.as_deref(),
        Some("showing lines 2-3 of 4, cut by limit=2; continue with offset 4")
    );
}

/// Both are the model naming the wrong thing, so both must be `bad_arguments`
/// and not `tool_failed`. Classed as a failure, a single typo would tell the
/// loop `Read` is broken and cost the model its main tool for the turn. The
/// directory case is here separately because the plausible wrong answer is to
/// list it, which quietly turns `Read` into a second `Glob` with no cap.
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

/// The alternative is `from_utf8_lossy`, which always succeeds and hands the
/// model replacement characters it cannot tell from real content — and if it
/// then edits and writes, the corruption is now on disk. Note the class:
/// `tool_failed`, not `bad_arguments`. The path was a perfectly good path and
/// the caller could not have known; it is the decode that failed.
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

// endregion: Read

// region: Glob and Grep
// ---------------------------------------------------------------------------
// Glob and Grep
//
// Emptiness is a result. Half of these assert that finding nothing succeeds;
// the other half assert that a search which genuinely could not run says so,
// because the two must never arrive looking the same.
// ---------------------------------------------------------------------------

/// "There are no `.proto` files here" is usually the fact the model went
/// looking for. Reported as an error it becomes "I could not look", and the
/// model spends a turn searching again another way for a problem it does not
/// have. `truncated` is asserted false as well: an empty answer that claims to
/// be partial is no answer at all.
#[tokio::test]
async fn glob_matching_nothing_succeeds_with_an_empty_list() {
    let sandbox = Sandbox::new();
    sandbox.write_file("a.rs", "fn main() {}\n");
    let outcome = sandbox.ok("Glob", json!({ "pattern": "**/*.ocaml" })).await;
    assert_eq!(outcome.content, "");
    assert!(!outcome.truncated);
}

/// Two claims in one. That `**/*.rs` reaches arbitrary depth — a matcher run
/// against the file name rather than the relative path passes the shallow case
/// and silently finds nothing nested. And that `.git` is not descended into:
/// the object store is large, opaque and never what anyone meant, and a repo
/// with real history would otherwise bury every genuine hit.
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

/// A pattern that will not compile is a fact about the call and needs no
/// filesystem to notice, so it must be caught before a tree is walked. Without
/// this the check can drift into the walk, where a malformed pattern becomes an
/// expensive traversal that matches nothing and reads, to the model, exactly
/// like "no such files exist".
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

/// The positive control across all three output modes, plus the two filters.
/// What it defends: `path:line:text` is the shape the model turns into its next
/// `Read` or `Edit`, so a missing line number costs a whole extra call per hit.
/// The `glob` assertion pins that the filter narrows rather than decorates —
/// `b.txt` contains the needle and must not appear.
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

/// One JPEG must not fail a search of the whole repository. `Read` refuses a
/// binary file because the caller named it; `Grep` merely walked past it, and
/// the honest report is that it has no lines rather than that the search broke.
/// Delete this and the first image in a real project turns every search into an
/// error the model then routes around.
#[tokio::test]
async fn grep_skips_binary_files_instead_of_failing_the_search() {
    let sandbox = Sandbox::new();
    sandbox.write_file("a.rs", "needle\n");
    std::fs::write(sandbox.root().join("blob.bin"), [0xff, 0xfe, 0x00]).unwrap();
    let outcome = sandbox.ok("Grep", json!({ "pattern": "needle" })).await;
    assert_eq!(outcome.content, "a.rs:1:needle");
}

/// Both must be distinguishable from "found nothing", which is the whole reason
/// `Grep` exists next to `Bash`. A broken pattern that returned an empty result
/// would tell the model the symbol is absent from the repository — the single
/// most expensive wrong answer this tool can give, because it looks like
/// information and ends the search.
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

// endregion: Glob and Grep

// region: Bash
// ---------------------------------------------------------------------------
// Bash
//
// The exit-status ruling, and the four things that keep a shell affordable:
// a real shell rather than an argv, a timeout that actually stops waiting, a
// cap that drains rather than blocks, and an environment holding no key.
// ---------------------------------------------------------------------------

/// The default `cwd` is the root, not wherever the Emma process happens to have
/// been started. Those coincide when the tests run and diverge in real use, so
/// this is the test that catches a `Command` built without `current_dir` — a
/// failure that would look like the agent reading somebody else's directory.
///
/// It is also the cheapest proof that whatever `resolve_shell` picked can
/// actually run something, which on Windows now means Git for Windows rather
/// than the WSL launcher: under WSL this printed a `/mnt/e/...` view of a
/// directory Emma had contained as `E:\...`, and the two only agreed by luck.
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
    //
    // It is here because "take an argv and exec it" is a genuinely tempting
    // hardening, and it fails silently: the model goes on writing shell
    // constructs, they become a single executable name with spaces in it, and
    // every one comes back "not found". This turns that into a red test rather
    // than a mystery in the transcript.
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

/// The contract test for the exit-status ruling, and the one that went red when
/// the ruling landed — which is the alarm working. Note it calls `ok`, not
/// `err`: a command that ran and exited 3 answered the question, so this fails
/// loudly if `Failed` ever comes back. What that would cost is not cosmetic —
/// the loop does not invoke a failed tool twice in one turn, so one honest
/// `grep -q` miss would take `Bash` away for the rest of the turn.
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

/// The other side of the same line: a command that never finished is a real
/// failure, because nothing answered. The wall-clock assertion is the important
/// half — a timeout that fires the error but does not stop waiting still hangs
/// the agent for the full `sleep 30`, and every assertion about the message
/// would pass while the session was unusable.
///
/// **This test takes ~30s of wall clock, and that is not the tool hanging.**
/// The `err(..)` call returns in well under a second — that is what the elapsed
/// assertion measures. What costs 30s is the *binary shutting down*: `sleep` is
/// a separate executable under a real POSIX shell, killing the shell orphans
/// it, it keeps the stdout pipe open for its full 30s, and the abandoned drain
/// task holds a blocking-pool thread the runtime waits on at shutdown. It cost
/// 2s while the shell was WSL — where killing the interop stub tore the whole
/// command down — so switching to Git for Windows is what made it visible. The
/// grandchild-outlives-the-kill case is the realistic one (any build tool does
/// this), so it is kept rather than traded for a faster suite.
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

/// A noisy build can spend an entire context window in one call. This pins both
/// halves of the fix: output is capped, and the cap is admitted. It also
/// exercises the drain-past-the-cap behaviour on purpose — a reader that
/// stopped reading at 64 KiB would fill the pipe buffer, the child would block
/// on a write nobody consumes, and this test would not fail, it would sit until
/// the timeout. That is why the payload is four times the cap: it has to
/// outlast the pipe buffer as well as the cap, or a broken reader finishes
/// anyway and the deadlock this exists to catch goes unnoticed.
///
/// **The timeout is a backstop, not a budget, and it is deliberately absurd.**
/// The reasoning above was written about a pipe-buffer deadlock, and it turned
/// out to describe an ordinary busy machine just as well: at 60s this failed
/// under a parallel test round while passing in isolation, which measured the
/// box rather than the code. So the work is now one `cat` of a file the test
/// wrote — a single fork, no 4,000-iteration shell loop — and the timeout is
/// five minutes. Nothing here should take milliseconds longer than that unless
/// the drain has genuinely stopped draining, which is the only failure the
/// clock is meant to catch.
#[tokio::test]
async fn bash_caps_output_and_says_so() {
    let sandbox = Sandbox::new();
    // 256 KiB: four times `MAX_STREAM_BYTES`, so the length assertion below is
    // one a missing cap would actually fail. The previous payload was 160 KB
    // against a 200 KiB bound, which no amount of cap regression could exceed.
    let line = "0123456789012345678901234567890123456789012345678901234567890123\n";
    sandbox.write_file("noisy.txt", &line.repeat(256 * 1024 / line.len()));

    let outcome = sandbox
        .ok(
            "Bash",
            json!({ "command": "cat noisy.txt", "timeout_ms": 300000 }),
        )
        .await;
    assert!(outcome.truncated, "256 KiB of output was not capped");
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

/// Emma is started with an API key in its environment and then runs commands
/// the model composed. Without `env_clear` and a rebuilt allowlist, `env` is an
/// exfiltration primitive that needs no filesystem access and trips no
/// containment check. The test sets a variable the allowlist does not name and
/// requires it to be absent — the direction that catches a filter-based
/// implementation, which lets through anything nobody thought to deny.
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

/// The positive control for `cwd`, paired with the refusal in
/// `containment.rs`. Containment tests alone are satisfied by a `cwd` that is
/// ignored entirely, which would drop every command into the root and quietly
/// change what relative paths in the command mean.
#[tokio::test]
async fn bash_starts_in_a_contained_subdirectory_when_asked() {
    let sandbox = Sandbox::new();
    sandbox.write_file("sub/inner.txt", "x");
    let outcome = sandbox
        .ok("Bash", json!({ "command": "ls", "cwd": "sub" }))
        .await;
    assert!(outcome.content.contains("inner.txt"), "{outcome:?}");
}

/// Whitespace, not an empty string — the case a `is_empty()` check misses.
/// Handed to `sh -c` it spawns a shell that does nothing and exits 0, so the
/// model gets a successful call, no output, and no idea its command was lost.
#[tokio::test]
async fn bash_refuses_an_empty_command() {
    let sandbox = Sandbox::new();
    let error = sandbox.err("Bash", json!({ "command": "   " })).await;
    assert_eq!(error.kind(), "bad_arguments");
}

/// The model is told which shell it is driving in the result, not in the
/// description — because the description is hashed into `tool_schema_hash`, and
/// a description that varied with whichever shell a machine happened to have
/// would make that hash machine-dependent and the attribution meaningless. So
/// the fact moves to run time, where it costs nothing and can be exact.
///
/// Asserted on a real spawn rather than on `resolve_shell()` alone: the banner
/// has to describe the shell that actually ran, and a banner assembled from a
/// second, independent resolution could disagree with it.
#[tokio::test]
async fn bash_names_the_shell_that_ran_in_the_result() {
    let sandbox = Sandbox::new();
    let outcome = sandbox.ok("Bash", json!({ "command": "echo hi" })).await;
    let first = outcome.content.lines().next().unwrap_or_default();
    assert!(first.starts_with("shell: "), "{outcome:?}");
    let shell = emma_tools_fs::resolve_shell().expect("a shell resolved");
    assert_eq!(first, shell.banner(), "{outcome:?}");
    assert!(outcome.content.contains("hi"), "{outcome:?}");
}

/// The banner must not displace the exit status, and the status must still be
/// the thing a skimming model sees before the output.
#[tokio::test]
async fn the_banner_sits_above_the_exit_status_not_instead_of_it() {
    let sandbox = Sandbox::new();
    let outcome = sandbox
        .ok("Bash", json!({ "command": "echo out; exit 3" }))
        .await;
    let mut lines = outcome.content.lines();
    assert!(lines.next().unwrap_or_default().starts_with("shell: "));
    assert_eq!(lines.next().unwrap_or_default(), "exit status 3");
    assert!(outcome.content.contains("out"), "{outcome:?}");
}

/// Whoever debugs "why did my command behave oddly" needs to see which shell
/// ran it without reading source, so the resolution is a public function rather
/// than something buried in `run`. This is the contract the command that prints
/// it is written against.
#[test]
fn the_resolved_shell_is_inspectable_from_outside() {
    let shell = emma_tools_fs::resolve_shell().expect("this box has a shell");
    assert!(shell.path.is_file(), "{shell}");
    assert_eq!(shell.kind, emma_tools_fs::ShellKind::Posix);
    // The live half of the WSL ruling, and the only place it can be checked
    // against a real machine: on Windows the automatic answer must not be the
    // launcher in the system directory, which is what a first-hit-on-PATH
    // search returns on a stock box.
    if cfg!(windows) {
        let lower = shell.path.to_string_lossy().to_lowercase();
        assert!(
            !lower.contains(r"\system32\") && !lower.contains(r"\windowsapps\"),
            "the automatic choice is the WSL launcher: {shell}"
        );
    } else {
        // The unix half of the same rule, which had no assertion at all: rule 1
        // of the documented order is `/bin/sh`, and off Windows that is not a
        // preference among candidates but the answer. Proved here only against
        // the fake filesystem in `bash.rs` otherwise, which cannot tell whether
        // the path it returns exists on this machine.
        assert_eq!(
            shell.path,
            std::path::Path::new("/bin/sh"),
            "the automatic choice off Windows must be /bin/sh: {shell}"
        );
    }
    let described = shell.to_string();
    assert!(described.contains("posix"), "{described}");
    assert!(
        described.contains(&shell.path.display().to_string()),
        "{described}"
    );
}

// endregion: Bash

// region: Shared behaviour
// ---------------------------------------------------------------------------
// Shared behaviour
//
// Properties that belong to no single tool: how a malformed call reads, and
// the session boundary the read tracker is keyed on.
// ---------------------------------------------------------------------------

/// A silently dropped parameter is indistinguishable, from the model's side,
/// from a flag that had no effect — so it concludes the behaviour is impossible
/// rather than that it misspelled the key, and stops trying. The message must
/// name the offending key, which is what turns a dead end into a retry.
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

/// Models correct a typed mistake reliably and guess at an untyped one. "Read
/// requires file_path" when a number was passed sends it looking for a missing
/// key it already supplied; naming what arrived closes the loop in one call.
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
    //
    // Delete this and the tracker can be keyed on the path alone — which every
    // other read-tracking test in this file would still accept, because they
    // all run inside one session. It is also the only test here that builds its
    // own `ToolCtx` rather than using the sandbox's, because the session id is
    // the thing under test.
    let sandbox = Sandbox::new();
    sandbox.write_file("s.txt", "one\n");
    sandbox.ok("Read", json!({ "file_path": "s.txt" })).await;

    let other = emma_tool_api::ToolCtx {
        cwd: sandbox.root().to_path_buf(),
        session_id: "a-later-session".into(),
        turn_id: "t".into(),
        background: Default::default(),
    };
    let error = sandbox
        .tool("Write")
        .invoke(&other, json!({ "file_path": "s.txt", "content": "two\n" }))
        .await
        .expect("no fault")
        .expect_err("the later session should not inherit the read");
    assert!(error.detail().contains("has not been read"), "{error}");
}

// endregion: Shared behaviour

/// A write leaves no temp file behind, and the target is never half-written.
///
/// `std::fs::write` truncates and then streams, so a crash, a full disk or a
/// kill between the two leaves somebody's source empty or partial. `Write` and
/// both `Edit` paths now rename a complete sibling over the target instead —
/// the pattern `tools/tasks` has used since it was written, and which the file
/// tools, which write far more often and to files nobody has a copy of, did
/// not have.
///
/// The stray-file half matters on its own: a `main.rs.emma-tmp` appearing in a
/// repository is a bug report even when the write succeeded.
#[tokio::test]
async fn a_write_leaves_no_temporary_file_beside_the_target() {
    let fs = Sandbox::new();
    fs.write_file("keep.txt", "before\n");

    fs.ok("Read", json!({ "file_path": "keep.txt" })).await;
    fs.ok(
        "Write",
        json!({ "file_path": "keep.txt", "content": "after\n" }),
    )
    .await;

    assert_eq!(fs.read_file("keep.txt"), "after\n");

    let strays: Vec<String> = std::fs::read_dir(fs.root())
        .expect("read the sandbox")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("tmp"))
        .collect();
    assert!(
        strays.is_empty(),
        "a temporary file survived the write: {strays:?}"
    );
}

/// Writing one file does not destroy another that happens to be named like the
/// temp file.
///
/// **This is a defect the atomic-write change introduced and an adversarial
/// review found.** The first version used a fixed `<name>.emma-tmp`, so a user
/// who owned `config.toml.emma-tmp` had it truncated and then renamed away by a
/// write to `config.toml` — this function destroying a file while claiming to
/// protect one. The temp name now carries the process id and a counter.
#[tokio::test]
async fn writing_a_file_does_not_clobber_a_sibling_named_like_the_temp_file() {
    let fs = Sandbox::new();
    fs.write_file("config.toml", "before\n");
    // A real file the user owns, whose name the old scheme would have taken.
    fs.write_file("config.toml.emma-tmp", "somebody elses file\n");

    fs.ok("Read", json!({ "file_path": "config.toml" })).await;
    fs.ok(
        "Write",
        json!({ "file_path": "config.toml", "content": "after\n" }),
    )
    .await;

    assert_eq!(fs.read_file("config.toml"), "after\n");
    assert_eq!(
        fs.read_file("config.toml.emma-tmp"),
        "somebody elses file\n",
        "writing one file destroyed another"
    );
}

/// A file too large to open is refused, with the cap and a route that works.
///
/// **The cap used to be output-side only.** `limit` bounds what is shown, and
/// the whole file was read into memory first regardless — so `Read` with
/// `limit: 1` on a very large file spent the whole file's worth of memory to
/// return one line. Not a truncation to report; work nobody asked for.
///
/// Refusal rather than a partial read, because `offset` and `limit` address
/// lines and a byte-capped read cannot honestly say which lines it missed.
#[tokio::test]
async fn a_file_over_the_read_limit_is_refused_with_the_cap_and_a_way_round_it() {
    let fs = Sandbox::new();
    // Just over 32 MiB, written once.
    let big = "x".repeat(1024 * 1024);
    let mut whole = String::new();
    for _ in 0..33 {
        whole.push_str(&big);
    }
    fs.write_file("huge.bin", &whole);

    let err = fs
        .err("Read", json!({ "file_path": "huge.bin", "limit": 1 }))
        .await;
    let shown = format!("{err:?}");
    assert!(shown.contains("was not opened"), "{shown}");
    assert!(shown.contains("No argument raises"), "{shown}");
    // And it names a route that actually works, rather than an argument that
    // does not exist — the mistake this repository already paid for once.
    assert!(shown.contains("Grep") || shown.contains("Bash"), "{shown}");
}

/// A file Windows tooling cannot delete is named at the moment it is created.
///
/// Emma's root canonicalises to the verbatim form, so `NUL` here is a real file
/// that round-trips rather than a write to the null device — the classic data
/// loss, which Emma does not have. The other side of that coin is that `cmd`,
/// Explorer and most tooling reach files through the non-verbatim API and
/// cannot open, move or delete such a name at all.
///
/// Said, not refused: the write worked and the content is retrievable through
/// Emma. Refusing a name the filesystem accepted would be Emma deciding what a
/// user may call a file.
#[cfg(windows)]
#[tokio::test]
async fn a_reserved_device_name_is_written_and_the_awkwardness_is_named() {
    let fs = Sandbox::new();
    let out = fs
        .ok("Write", json!({ "file_path": "NUL", "content": "kept\n" }))
        .await;
    let shown = format!("{out:?}");
    assert!(shown.contains("reserved device name"), "{shown}");
    assert!(shown.contains("cannot"), "{shown}");
    // And it really is a file, not a write into the void — read back through
    // Emma, which is the claim the note actually makes. Reading it with plain
    // `std::fs` gets the null device and an empty string, which is the whole
    // reason the note exists: the two APIs disagree about what this name means.
    let back = fs.ok("Read", json!({ "file_path": "NUL" })).await;
    assert!(
        format!("{back:?}").contains("kept"),
        "Emma could not read back the file it just wrote: {back:?}"
    );

    // An ordinary name says nothing, or the note becomes noise.
    let out = fs
        .ok(
            "Write",
            json!({ "file_path": "plain.txt", "content": "x\n" }),
        )
        .await;
    assert!(!format!("{out:?}").contains("note:"), "{out:?}");
}

// region: Hard links
// ---------------------------------------------------------------------------
// Hard links
//
// The atomic write that DEF-006 added replaces the directory entry instead of
// writing through it. That is what makes it atomic and it is also what breaks a
// hard link: the edited name gets the new content, the other name keeps the
// old, and the count drops to one on each. Most editors do the same and a torn
// file on a crash is worse, so the write stays as it is and the outcome says
// what happened.
// ---------------------------------------------------------------------------

/// Without this, `Write` goes back to breaking a hard link in silence.
///
/// **The severance is asserted as well as the sentence**, because a note that
/// described something the write did not do would be its own defect, and a note
/// asserted alone cannot tell the two apart. Both names are read back: the
/// written one changed, the other did not.
#[tokio::test]
async fn write_says_when_it_has_just_broken_a_hard_link() {
    let sandbox = Sandbox::new();
    sandbox.write_file("linked.txt", "original\n");
    let one = sandbox.root().join("linked.txt");
    let two = sandbox.root().join("other-name.txt");
    if std::fs::hard_link(&one, &two).is_err() {
        // Some filesystems have no hard links at all. Skipping loudly beats a
        // test that quietly asserts nothing on a machine that cannot host it.
        eprintln!("skipped: this filesystem refused a hard link");
        return;
    }

    sandbox
        .ok("Read", json!({ "file_path": "linked.txt" }))
        .await;
    let out = sandbox
        .ok(
            "Write",
            json!({ "file_path": "linked.txt", "content": "rewritten\n" }),
        )
        .await;

    assert!(
        out.content.contains("hard links"),
        "the outcome did not say the link was broken: {out:?}"
    );
    assert!(
        out.content.contains("2 names"),
        "the count is what makes it checkable: {out:?}"
    );
    assert_eq!(std::fs::read_to_string(&one).unwrap(), "rewritten\n");
    assert_eq!(
        std::fs::read_to_string(&two).unwrap(),
        "original\n",
        "if this now says `rewritten` the write stopped being atomic and the note is wrong"
    );
}

/// Without this, the note fires on ordinary files and becomes noise nobody
/// reads — which is the same as not having it.
#[tokio::test]
async fn an_ordinary_write_says_nothing_about_links() {
    let sandbox = Sandbox::new();
    sandbox.write_file("plain.txt", "original\n");
    sandbox
        .ok("Read", json!({ "file_path": "plain.txt" }))
        .await;

    let out = sandbox
        .ok(
            "Write",
            json!({ "file_path": "plain.txt", "content": "rewritten\n" }),
        )
        .await;

    assert!(
        !out.content.contains("hard link"),
        "a file with one name was told it had more: {out:?}"
    );
}

/// Without this, `Edit` keeps the silence `Write` just lost. It goes through
/// the same `write_atomically` and breaks the link exactly as hard.
#[tokio::test]
async fn edit_says_it_too() {
    let sandbox = Sandbox::new();
    sandbox.write_file("linked.rs", "let x = 1;\n");
    let one = sandbox.root().join("linked.rs");
    let two = sandbox.root().join("linked-elsewhere.rs");
    if std::fs::hard_link(&one, &two).is_err() {
        eprintln!("skipped: this filesystem refused a hard link");
        return;
    }

    sandbox
        .ok("Read", json!({ "file_path": "linked.rs" }))
        .await;
    let out = sandbox
        .ok(
            "Edit",
            json!({ "file_path": "linked.rs", "old_string": "1", "new_string": "9" }),
        )
        .await;

    assert!(
        out.content.contains("hard links"),
        "Edit broke the link without saying so: {out:?}"
    );
    assert_eq!(std::fs::read_to_string(&two).unwrap(), "let x = 1;\n");
}

// endregion: Hard links

// region: A directory the search could not open
// ---------------------------------------------------------------------------
// A directory the search could not open
//
// `DEF-002` made an unreadable FILE counted and named. The walk above it still
// discarded an unreadable DIRECTORY in silence, so `Grep` answered `no matches`
// about a file it had never opened — certified live by a reviewer who denied
// read on a subdirectory and watched a matching file disappear from the results
// with no count and no cut notice.
// ---------------------------------------------------------------------------

/// Without this, `Grep` goes back to reporting a complete search over a tree it
/// could not fully read.
///
/// **The permission change is the test.** A fixture cannot produce this: the
/// walk only fails when the operating system refuses, so the only honest way to
/// exercise it is to make the operating system refuse. Where that cannot be
/// arranged the test says so and stops, rather than passing on a tree it could
/// read perfectly well.
#[tokio::test]
async fn grep_says_when_a_directory_could_not_be_opened() {
    let sandbox = Sandbox::new();
    sandbox.write_file("visible.txt", "NEEDLE here\n");
    sandbox.write_file("secret/hidden.txt", "NEEDLE hidden\n");
    let secret = sandbox.root().join("secret");

    if !deny_read(&secret) {
        eprintln!("SKIPPED: this platform would not make a directory unreadable");
        return;
    }

    let out = sandbox.ok("Grep", json!({ "pattern": "NEEDLE" })).await;
    let _ = allow_read(&secret);

    assert!(
        out.content.contains("could not be opened"),
        "the search skipped a directory and reported a complete result: {:?}",
        out.content
    );
    assert!(
        out.content.contains("NOT searched"),
        "the notice must say the tree was not searched, not merely that something happened: {:?}",
        out.content
    );
    assert!(
        out.content.contains("secret"),
        "a count without the name sends the reader nowhere: {:?}",
        out.content
    );
    // And the part that makes the notice worth having: the search still
    // returned what it could. An unreadable directory is not a failed call.
    assert!(
        out.content.contains("visible.txt"),
        "one permission bit hid the whole search: {:?}",
        out.content
    );
}

/// Without this, the notice fires on an ordinary tree and becomes noise, which
/// is the same as not having it.
#[tokio::test]
async fn an_ordinary_search_says_nothing_about_unreadable_directories() {
    let sandbox = Sandbox::new();
    sandbox.write_file("visible.txt", "NEEDLE here\n");
    sandbox.write_file("sub/other.txt", "NEEDLE too\n");

    let out = sandbox.ok("Grep", json!({ "pattern": "NEEDLE" })).await;
    assert!(
        !out.content.contains("could not be opened"),
        "a readable tree was reported as partly unreadable: {:?}",
        out.content
    );
}

/// Make a directory unreadable, or say we could not.
///
/// Windows only today, through `icacls`. There is no portable way to do this,
/// and a `#[cfg(unix)]` arm asserted from a Windows box would be the fake this
/// project keeps filing — the unix arm goes in when a unix box runs the suite.
fn deny_read(dir: &std::path::Path) -> bool {
    if !cfg!(windows) {
        return false;
    }
    let user = std::env::var("USERNAME").unwrap_or_default();
    if user.is_empty() {
        return false;
    }
    std::process::Command::new("icacls")
        .arg(dir)
        .arg("/deny")
        .arg(format!("{user}:(OI)(CI)(RX,RD,GR)"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|st| st.success())
        .unwrap_or(false)
        && std::fs::read_dir(dir).is_err()
}

/// Put the permission back, so the sandbox can be removed.
fn allow_read(dir: &std::path::Path) -> bool {
    let user = std::env::var("USERNAME").unwrap_or_default();
    std::process::Command::new("icacls")
        .arg(dir)
        .arg("/remove:d")
        .arg(&user)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|st| st.success())
        .unwrap_or(false)
}

// endregion: A directory the search could not open
