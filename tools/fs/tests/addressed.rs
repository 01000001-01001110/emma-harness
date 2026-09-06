//! `Edit.lines` — the guarantee is the refusal.
//!
//! The positive cases here are cheap and mostly obvious. The tests worth their
//! line count are the ones where a file changed between the `Read` and the
//! `Edit`, because that is the case the literal-string form mishandles
//! silently: `old_string` still matches text that drifted somewhere else in the
//! file, and the edit lands where nobody meant it to. Every refusal below
//! asserts two things — that the edit did **not** happen, and that the message
//! names enough for the model to recover without re-reading the file. The
//! second half matters as much as the first. A refusal the model cannot act on
//! is just a re-read with extra steps, and avoiding the re-read is the entire
//! reason this addressing form exists.
//!
//! Each test carries a line saying what breaks if it is deleted, because a test
//! whose purpose is only legible from its assertions is one a later reader will
//! weaken to make a change compile.

mod support;

use emma_tools_fs::hashline;
use serde_json::json;
use support::Sandbox;

/// The label `Read` printed for `line`, as the model would copy it.
///
/// Going through `Read`'s real output rather than computing the hash directly
/// is deliberate for most tests here: it means these exercise the same
/// round-trip the model does, so a `Read` that started labelling lines with
/// hashes `Edit` will not accept fails here rather than in production.
async fn label(sandbox: &Sandbox, path: &str, line: usize) -> String {
    let outcome = sandbox.ok("Read", json!({ "file_path": path })).await;
    let prefix = format!("{line:>6}#");
    let found = outcome
        .content
        .lines()
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no line {line} in:\n{}", outcome.content));
    let (head, _) = found.split_once('\t').expect("label ends at the tab");
    head.trim().to_string()
}

// region: It works
// ---------------------------------------------------------------------------
// It works
//
// The positive controls. Everything in the next region asserts a refusal, and a
// tool that refused every addressed edit would satisfy all of them.
// ---------------------------------------------------------------------------

/// The positive control for the whole scheme. Without it, the safe way to make
/// every refusal test pass is to refuse every addressed edit, and `Edit.lines`
/// becomes a parameter that has never once changed a file.
#[tokio::test]
async fn an_addressed_edit_changes_exactly_the_line_named() {
    let sandbox = Sandbox::new();
    sandbox.write_file("one.rs", "alpha\nbeta\ngamma\n");
    let at = label(&sandbox, "one.rs", 2).await;

    let outcome = sandbox
        .ok(
            "Edit",
            json!({ "file_path": "one.rs", "lines": at, "new_string": "BETA" }),
        )
        .await;

    assert_eq!(sandbox.read_file("one.rs"), "alpha\nBETA\ngamma\n");
    assert!(outcome.content.contains("replaced line 2"), "{outcome:?}");
}

/// A range is what makes the form worth having for anything bigger than a
/// typo, and the shift report is what stops the *next* edit landing wrong.
/// Delete this and a range could quietly replace only its first line, or
/// replace the right lines while telling the model the numbering is unchanged.
#[tokio::test]
async fn a_range_replaces_all_of_it_and_says_how_far_the_rest_moved() {
    let sandbox = Sandbox::new();
    sandbox.write_file("r.rs", "a\nb\nc\nd\ne\n");
    let from = label(&sandbox, "r.rs", 2).await;
    let to = label(&sandbox, "r.rs", 4).await;

    let outcome = sandbox
        .ok(
            "Edit",
            json!({ "file_path": "r.rs", "lines": format!("{from}-{to}"), "new_string": "X" }),
        )
        .await;

    assert_eq!(sandbox.read_file("r.rs"), "a\nX\ne\n");
    assert!(
        outcome.content.contains("moved up by 2"),
        "the model's other line numbers just moved and nothing else tells it: {outcome:?}"
    );
}

/// The saving this form claims is that a second edit needs no second `Read`.
/// That claim is only true if the result hands back usable labels, so this
/// makes the second edit *from the first edit's output* with no `Read` between
/// them. Delete it and the labels can rot into decoration — still printed,
/// no longer accepted — and nothing would notice.
#[tokio::test]
async fn the_result_labels_its_new_lines_so_the_next_edit_needs_no_read() {
    let sandbox = Sandbox::new();
    sandbox.write_file("chain.rs", "one\ntwo\nthree\n");
    let at = label(&sandbox, "chain.rs", 2).await;

    let first = sandbox
        .ok(
            "Edit",
            json!({ "file_path": "chain.rs", "lines": at, "new_string": "TWO" }),
        )
        .await;

    // Exactly what the model has in front of it: the label out of the result.
    let echoed = first
        .content
        .lines()
        .find(|l| l.contains("\tTWO"))
        .and_then(|l| l.split_once('\t'))
        .map(|(head, _)| head.trim().to_string())
        .unwrap_or_else(|| {
            panic!(
                "the result did not label its own output:\n{}",
                first.content
            )
        });

    sandbox
        .ok(
            "Edit",
            json!({ "file_path": "chain.rs", "lines": echoed, "new_string": "2" }),
        )
        .await;
    assert_eq!(sandbox.read_file("chain.rs"), "one\n2\nthree\n");
}

/// Deletion has to be reachable, and "replace with nothing" is the only
/// spelling available. Delete this and an empty `new_string` may quietly start
/// leaving a blank line behind — a change the model cannot see in the result
/// and will only find later in a diff.
#[tokio::test]
async fn an_empty_replacement_deletes_the_lines_and_says_so() {
    let sandbox = Sandbox::new();
    sandbox.write_file("d.rs", "keep\ndrop\nkeep\n");
    let at = label(&sandbox, "d.rs", 2).await;

    let outcome = sandbox
        .ok(
            "Edit",
            json!({ "file_path": "d.rs", "lines": at, "new_string": "" }),
        )
        .await;

    assert_eq!(sandbox.read_file("d.rs"), "keep\nkeep\n");
    assert!(outcome.content.contains("deleted line 2"), "{outcome:?}");
}

/// Rebuilding the file from `str::lines` would convert every CRLF file to LF on
/// the first one-line edit, and would add a newline to the end of a file that
/// never had one. Both are whole-file changes disguised as a one-line change:
/// they blow up the diff the user is asked to approve, and on Windows they turn
/// every edit into a mass rewrite. Delete this and nothing else in the suite
/// looks at a byte that is not `\n`.
#[tokio::test]
async fn crlf_and_a_missing_final_newline_both_survive() {
    let sandbox = Sandbox::new();
    sandbox.write_file("w.txt", "a\r\nb\r\nc");
    let second = label(&sandbox, "w.txt", 2).await;
    sandbox
        .ok(
            "Edit",
            json!({ "file_path": "w.txt", "lines": second, "new_string": "B" }),
        )
        .await;
    assert_eq!(sandbox.read_file("w.txt"), "a\r\nB\r\nc");

    // The last line, which has no terminator to copy.
    let third = label(&sandbox, "w.txt", 3).await;
    let outcome = sandbox
        .ok(
            "Edit",
            json!({ "file_path": "w.txt", "lines": third, "new_string": "C" }),
        )
        .await;
    assert_eq!(sandbox.read_file("w.txt"), "a\r\nB\r\nC");
    assert!(
        outcome.content.contains("no newline at the end"),
        "a model that assumes a final newline will build its next edit wrong: {outcome:?}"
    );
}

// endregion: It works

// region: The file changed underneath
// ---------------------------------------------------------------------------
// The file changed underneath
//
// The reason the scheme exists. In every test here the user — or a formatter,
// or another agent — has written the file between the `Read` and the `Edit`,
// which is the ordinary case on a machine where somebody has the file open in
// an editor. Each asserts that the edit did not happen and that the message is
// enough to fix the call.
// ---------------------------------------------------------------------------

/// **The guard.** An anchor line that changed underneath the agent is refused,
/// never applied to whatever is there now. Delete this and the hash becomes
/// decoration: `Edit.lines` degrades to editing by bare line number, which
/// silently rewrites the wrong line every time somebody saves a file while the
/// agent is thinking. Nothing else in this suite would go red.
#[tokio::test]
async fn an_edit_whose_line_changed_underneath_is_refused_not_applied() {
    let sandbox = Sandbox::new();
    sandbox.write_file("app.rs", "fn main() {\n    let user = legacy(req);\n}\n");
    let at = label(&sandbox, "app.rs", 2).await;

    // The user saves the file in their editor. Same line count, different line.
    sandbox.write_file("app.rs", "fn main() {\n    let user = other(req);\n}\n");

    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "app.rs", "lines": at, "new_string": "    let user = current(req)?;" }),
        )
        .await;

    assert_eq!(error.kind(), "bad_arguments");
    assert_eq!(
        sandbox.read_file("app.rs"),
        "fn main() {\n    let user = other(req);\n}\n",
        "the refused edit was applied to the line that took its place"
    );
    let detail = error.detail();
    assert!(detail.contains("line 2"), "which line: {detail}");
    assert!(
        detail.contains("let user = other(req);"),
        "what is there now: {detail}"
    );
    assert!(detail.contains(&at), "what was addressed: {detail}");
}

/// The recovery half, and the reason the refusal is worth more than a bare
/// "stale". When the text simply moved — an import added above it, the usual
/// case — the message names the line it moved to, and the model can retry
/// without spending a whole `Read`. Delete this and the refusal is still
/// correct and the model still pays for a re-read every time, which is the cost
/// this whole change exists to remove.
#[tokio::test]
async fn the_refusal_says_where_the_line_went() {
    let sandbox = Sandbox::new();
    sandbox.write_file("m.rs", "alpha\nbeta\ngamma\n");
    let at = label(&sandbox, "m.rs", 2).await;

    // Somebody adds a line at the top; `beta` is now line 3.
    sandbox.write_file("m.rs", "use std::io;\nalpha\nbeta\ngamma\n");

    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "m.rs", "lines": at, "new_string": "BETA" }),
        )
        .await;

    let detail = error.detail();
    assert!(
        detail.contains("line 3"),
        "the message must name where it went, or the model can only re-read: {detail}"
    );
    let hash = at.split_once('#').unwrap().1;
    assert!(
        detail.contains(&format!("3#{hash}")),
        "and must spell the corrected address: {detail}"
    );
    // The wording, which is as load-bearing as the content and was rewritten
    // after a real model read the first draft as its own counting mistake and
    // paid for a full re-read anyway. See `moved` in edit.rs. Both halves are
    // pinned: the message says the *file* changed, and it does not offer the
    // re-read as an equal alternative to the corrected address.
    assert!(
        detail.contains("changed on disk after you read it"),
        "the model must be told the cause, or it reads this as having miscounted: {detail}"
    );
    assert!(
        detail.contains("retry with") && detail.contains("only need to Read"),
        "the corrected address must be the instruction and the re-read the narrower \
         case, or the model will re-read every time: {detail}"
    );
    assert_eq!(
        sandbox.read_file("m.rs"),
        "use std::io;\nalpha\nbeta\ngamma\n"
    );
}

/// **The payoff.** A file that drifted somewhere *else* no longer costs a full
/// re-read: the addressed lines are verified individually, so an edit whose
/// target is untouched goes through. Delete this and the obvious way to make
/// every refusal above pass is to refuse any edit to a file that changed at
/// all — which is exactly the old behaviour, and gives up the entire saving.
/// The `old_string` half of the assertion is the control: that form still
/// refuses, deliberately, because a quoted anchor can match text that moved.
#[tokio::test]
async fn drift_elsewhere_in_the_file_does_not_block_an_addressed_edit() {
    let sandbox = Sandbox::new();
    sandbox.write_file("far.rs", "one\ntwo\nthree\n");
    let at = label(&sandbox, "far.rs", 3).await;

    // Line 1 changes; line 3 does not.
    sandbox.write_file("far.rs", "ONE ELEVEN\ntwo\nthree\n");

    let stale = sandbox
        .err(
            "Edit",
            json!({ "file_path": "far.rs", "old_string": "three", "new_string": "THREE" }),
        )
        .await;
    assert!(stale.detail().contains("changed on disk"), "{stale}");
    assert!(
        stale.detail().contains("Edit.lines"),
        "the refusal should point at the form that would have worked: {stale}"
    );

    sandbox
        .ok(
            "Edit",
            json!({ "file_path": "far.rs", "lines": at, "new_string": "THREE" }),
        )
        .await;
    assert_eq!(sandbox.read_file("far.rs"), "ONE ELEVEN\ntwo\nTHREE\n");
}

/// **The other half of the relocation hint, and it exists because the first
/// version of this shipped a contradiction.** The refusal tells the model "your
/// line is now line 7, retry there" — and the range check, which originally
/// compared the recorded hash for line 7 against the file's line 7, refused the
/// retry, because a line inserted above had shifted every number below it. A
/// real model took the advice, was refused for following it, and paid for the
/// full re-read anyway. Delete this and that contradiction comes straight back:
/// every other test here passes with an in-place comparison, because none of
/// them edits a file whose lines have *moved* rather than changed.
#[tokio::test]
async fn a_line_that_only_moved_can_be_edited_at_its_new_number() {
    let sandbox = Sandbox::new();
    sandbox.write_file("shift.rs", "alpha\nbeta\ngamma\n");
    let at = label(&sandbox, "shift.rs", 2).await;
    let hash = at.split_once('#').unwrap().1.to_string();

    // Two imports arrive at the top. `beta` is untouched and is now line 4.
    sandbox.write_file("shift.rs", "use a;\nuse b;\nalpha\nbeta\ngamma\n");

    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "shift.rs", "lines": at, "new_string": "BETA" }),
        )
        .await;
    assert!(error.detail().contains("is now line 4"), "{error}");

    // Exactly what that refusal told the model to do, and it must work.
    sandbox
        .ok(
            "Edit",
            json!({ "file_path": "shift.rs", "lines": format!("4#{hash}"), "new_string": "BETA" }),
        )
        .await;
    assert_eq!(
        sandbox.read_file("shift.rs"),
        "use a;\nuse b;\nalpha\nBETA\ngamma\n"
    );
}

/// The hole that per-line hashing has and whole-file hashing does not: a range
/// names only its endpoints, so a line *between* them can drift with both
/// endpoints intact. It is closed by the tracker's own record rather than by
/// making the model send a hash per line. Delete this and a range edit will
/// happily swallow somebody else's change to its middle — the exact silent
/// data loss the scheme is sold as preventing, and invisible in the result.
#[tokio::test]
async fn drift_inside_a_range_is_caught_though_only_the_ends_were_named() {
    let sandbox = Sandbox::new();
    sandbox.write_file("mid.rs", "a\nb\nc\nd\ne\n");
    let from = label(&sandbox, "mid.rs", 2).await;
    let to = label(&sandbox, "mid.rs", 4).await;

    // Line 3 changes. Lines 2 and 4 — the two the model named — do not.
    sandbox.write_file("mid.rs", "a\nb\nCCC\nd\ne\n");

    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "mid.rs", "lines": format!("{from}-{to}"), "new_string": "X" }),
        )
        .await;

    assert_eq!(sandbox.read_file("mid.rs"), "a\nb\nCCC\nd\ne\n");
    let detail = error.detail();
    assert!(
        detail.contains("line 3"),
        "naming the drifted line is what lets the model look at just that line: {detail}"
    );
    assert!(detail.contains("CCC"), "and what it says now: {detail}");
}

/// A file that got shorter is a different problem from a line that changed, and
/// telling the model "hash mismatch" for it would be a lie about what happened —
/// there is no line there at all. Delete this and an address past the end panics
/// on the index instead, which ends the turn rather than costing a retry.
#[tokio::test]
async fn an_address_past_the_end_names_the_length_instead_of_panicking() {
    let sandbox = Sandbox::new();
    sandbox.write_file("short.rs", "a\nb\nc\n");
    let at = label(&sandbox, "short.rs", 3).await;
    sandbox.write_file("short.rs", "a\n");

    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "short.rs", "lines": at, "new_string": "C" }),
        )
        .await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("has 1 lines"), "{error}");
}

// endregion: The file changed underneath

// region: What may not be addressed
// ---------------------------------------------------------------------------
// What may not be addressed
//
// The read requirement, at line resolution. `Write` refuses a file nobody has
// read; this refuses a *line* nobody has been shown, which is the same rule
// applied where an addressed edit can actually reach.
// ---------------------------------------------------------------------------

/// The same rule `old_string` obeys, and it has to be stated separately because
/// the addressed form takes a different path through the tool. Delete this and
/// `Edit.lines` becomes the way around the read requirement: guess a line
/// number, guess four hex characters, write to a file nobody has looked at.
#[tokio::test]
async fn a_file_that_was_never_read_cannot_be_addressed() {
    let sandbox = Sandbox::new();
    sandbox.write_file("unseen.rs", "alpha\n");
    let real = hashline::short(hashline::hash_line("alpha"));

    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "unseen.rs", "lines": format!("1#{real}"), "new_string": "beta" }),
        )
        .await;
    assert!(error.detail().contains("has not been read"), "{error}");
    assert_eq!(sandbox.read_file("unseen.rs"), "alpha\n");
}

/// A paged read shows a window, and the lines outside it are exactly as unseen
/// as a file that was never opened. The hash here is the *correct* one,
/// computed directly, so this cannot pass by accident: it is refused because
/// nobody showed the model that line, not because the hash was wrong. Delete
/// this and reading the first page of a file licenses blind edits to the rest
/// of it.
#[tokio::test]
async fn a_line_outside_the_window_that_was_read_cannot_be_addressed() {
    let sandbox = Sandbox::new();
    sandbox.write_file("page.rs", "l1\nl2\nl3\nl4\nl5\n");
    sandbox
        .ok(
            "Read",
            json!({ "file_path": "page.rs", "offset": 1, "limit": 2 }),
        )
        .await;

    let real = hashline::short(hashline::hash_line("l4"));
    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "page.rs", "lines": format!("4#{real}"), "new_string": "L4" }),
        )
        .await;
    assert!(
        error
            .detail()
            .contains("outside the part of the file you have read"),
        "{error}"
    );
    assert_eq!(sandbox.read_file("page.rs"), "l1\nl2\nl3\nl4\nl5\n");

    // ...and the window itself is still editable, or the rule would just be
    // "a paged read licenses nothing", which is a different and worse tool.
    let inside = label(&sandbox, "page.rs", 1).await;
    sandbox
        .ok(
            "Edit",
            json!({ "file_path": "page.rs", "lines": inside, "new_string": "L1" }),
        )
        .await;
    assert_eq!(sandbox.read_file("page.rs"), "L1\nl2\nl3\nl4\nl5\n");
}

/// A clipped line is one whose end nobody has seen, so replacing it destroys
/// text the model never read — the one-line version of the whole-file clobber
/// `Write` exists to refuse. Both doors are checked: the `----` marker `Read`
/// prints in place of a hash, and the real hash, which a model could in
/// principle obtain some other way. Delete this and `Read`'s clipping cap
/// quietly becomes a way to lose the tail of a long line.
#[tokio::test]
async fn a_line_too_long_to_have_been_shown_whole_cannot_be_addressed() {
    let sandbox = Sandbox::new();
    let long = "x".repeat(2500);
    sandbox.write_file("long.txt", &format!("short\n{long}\n"));
    let read = sandbox.ok("Read", json!({ "file_path": "long.txt" })).await;
    assert!(
        read.content.contains("2#----"),
        "Read must mark the line it could not show whole:\n{}",
        read.content
    );

    // The marker, copied straight out of the read.
    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "long.txt", "lines": "2#----", "new_string": "y" }),
        )
        .await;
    assert!(error.detail().contains("clip"), "{error}");

    // And the real hash, which must not be a way round it either.
    let real = hashline::short(hashline::hash_line(&long));
    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "long.txt", "lines": format!("2#{real}"), "new_string": "y" }),
        )
        .await;
    assert!(
        error.detail().contains("have not seen the end of it"),
        "{error}"
    );
    assert_eq!(sandbox.read_file("long.txt"), format!("short\n{long}\n"));
}

// endregion: What may not be addressed

// region: Malformed calls
// ---------------------------------------------------------------------------
// Malformed calls
//
// Two ways of saying where means a new class of mistake: saying both, saying
// neither, or bringing a flag that belongs to the other form. All three are
// refused rather than resolved by precedence, because resolving them means
// silently ignoring something the model asked for.
// ---------------------------------------------------------------------------

/// Saying neither, or bringing the wrong form's flag, is still settled from the
/// arguments alone. Delete this and those migrate into the file-touching half of
/// the tool, where a missing file masks an argument mistake.
///
/// **Saying BOTH deliberately no longer lives here.** It used to, and the
/// original note warned that removing it would buy a precedence rule where a
/// model that supplied both got a successful edit somewhere it did not intend.
/// That warning is still correct and is still honoured: there is no precedence.
/// An address plus a quote is accepted only when the two name the same text, so
/// nothing is thrown away, and a disagreement is still refused — with the
/// addressed text in the message. Judging agreement needs the file, so the check
/// cannot be settled before the disk, and that trade is deliberate: the blanket
/// refusal fired 14 times across recorded runs and every run it touched failed.
/// See `both_forms_that_agree_name_one_place`.
#[tokio::test]
async fn saying_neither_or_the_wrong_flag_is_refused_before_the_disk() {
    let sandbox = Sandbox::new();

    // A path that does not exist, which proves these are settled from the
    // arguments alone: if either check migrates into the file-touching half of
    // the tool, this goes red.
    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "absent.rs", "new_string": "b" }),
        )
        .await;
    assert!(
        error.detail().contains("needs somewhere to change"),
        "{error}"
    );

    // `replace_all` means "all of them", which an address has already answered.
    // Accepting and ignoring it would teach the model the flag does nothing.
    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "absent.rs", "lines": "1#0000", "new_string": "b", "replace_all": true }),
        )
        .await;
    assert!(error.detail().contains("old_string only"), "{error}");
}

/// Both forms, agreeing. The redundancy is the model quoting the lines it just
/// addressed, which is what they actually do; refusing it cost 14 calls across
/// recorded runs. There is no precedence here: the two say the same thing, so
/// the address is used and nothing the model asked for is discarded.
#[tokio::test]
async fn both_forms_that_agree_name_one_place() {
    let sandbox = Sandbox::new();
    sandbox.write_file("a.rs", "alpha\nbeta\ngamma\n");
    // Through Read's real output, like every other test here.
    let label = label(&sandbox, "a.rs", 2).await;

    sandbox
        .ok(
            "Edit",
            json!({ "file_path": "a.rs", "lines": label, "old_string": "beta", "new_string": "BETA" }),
        )
        .await;
    assert_eq!(sandbox.read_file("a.rs"), "alpha\nBETA\ngamma\n");
}

/// Both forms, disagreeing. Still refused, because this is the case the original
/// warning was about, and the message has to say which text the address covers
/// or the model cannot tell which of its two instructions was wrong.
#[tokio::test]
async fn both_forms_that_disagree_are_still_refused() {
    let sandbox = Sandbox::new();
    sandbox.write_file("a.rs", "alpha\nbeta\ngamma\n");
    // Through Read's real output, like every other test here.
    let label = label(&sandbox, "a.rs", 2).await;

    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "a.rs", "lines": label, "old_string": "gamma", "new_string": "X" }),
        )
        .await;
    assert!(error.detail().contains("disagree"), "{error}");
    assert!(
        error.detail().contains("beta"),
        "must show the addressed text: {error}"
    );
    assert_eq!(
        sandbox.read_file("a.rs"),
        "alpha\nbeta\ngamma\n",
        "no write on refusal"
    );
}

/// The syntax the model has to construct rather than copy, so its errors have
/// to teach. A bare line number is the likely mistake and gets its own message
/// pointing at `Read`'s output; the rest name what was wrong. Delete this and
/// a mistyped address returns "invalid", which is a guess-and-retry loop.
#[tokio::test]
async fn a_malformed_address_says_what_would_have_worked() {
    let sandbox = Sandbox::new();
    sandbox.write_file("a.rs", "x\n");

    for (bad, expect) in [
        ("12", "no #hash"),
        ("12#zzzz", "four hex characters"),
        ("0#a3f9", "line numbers start at 1"),
        ("9#a3f9-2#b7c1", "ends before it starts"),
    ] {
        let error = sandbox
            .err(
                "Edit",
                json!({ "file_path": "a.rs", "lines": bad, "new_string": "y" }),
            )
            .await;
        assert!(
            error.detail().contains(expect),
            "{bad:?} should have been explained with {expect:?}: {error}"
        );
    }
}

/// `Edit` is not idempotent and says so; a replacement identical to what is
/// already there is the model believing it changed something it did not, and
/// everything it does next is built on that belief. The `old_string` form
/// refuses the same thing from the arguments alone — this is the addressed
/// form's version, which can only be known once the file is read.
#[tokio::test]
async fn a_replacement_that_would_change_nothing_is_refused() {
    let sandbox = Sandbox::new();
    sandbox.write_file("same.rs", "alpha\nbeta\n");
    let at = label(&sandbox, "same.rs", 2).await;

    let error = sandbox
        .err(
            "Edit",
            json!({ "file_path": "same.rs", "lines": at, "new_string": "beta" }),
        )
        .await;
    assert!(error.detail().contains("nothing would change"), "{error}");
}

// endregion: Malformed calls

// region: Read's labels inside the arguments
// ---------------------------------------------------------------------------
// Read's labels inside the arguments
//
// `edit.rs` grew two fallbacks for models that echo `Read`'s `12#a3f9\t` labels
// back inside `old_string` and `new_string`. Both were pinned only by unit
// tests over `strip_read_labels` itself, which stay green with the fallbacks
// deleted from `by_old_string` — a helper proved correct and never called. The
// two tests below drive the whole tool, so deleting either fallback goes red
// here.
// ---------------------------------------------------------------------------

/// `Read`'s rendering of one line, label and tab included, exactly as a model
/// copying out of the transcript would paste it.
async fn labelled_line(sandbox: &Sandbox, path: &str, line: usize) -> String {
    let outcome = sandbox.ok("Read", json!({ "file_path": path })).await;
    let prefix = format!("{line:>6}#");
    outcome
        .content
        .lines()
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no line {line} in:\n{}", outcome.content))
        .to_string()
}

/// Measured 2026-08-24: nemotron-3.5-lightning:30b-mlx sent eight labelled
/// anchors in one run and every one missed a file it had read correctly.
/// Refusing that is technically right and useless — the model quoted the bytes
/// the harness printed. Delete the `relabelled` fallback in `by_old_string` and
/// this goes red; the literal path is untouched, so a correct anchor never
/// reaches it.
#[tokio::test]
async fn an_old_string_carrying_reads_labels_still_finds_its_anchor() {
    let sandbox = Sandbox::new();
    sandbox.write_file("a.rs", "alpha\nbeta\ngamma\n");
    let quoted = labelled_line(&sandbox, "a.rs", 2).await;
    assert!(quoted.contains('#') && quoted.contains('\t'), "{quoted:?}");

    sandbox
        .ok(
            "Edit",
            json!({ "file_path": "a.rs", "old_string": quoted, "new_string": "BETA" }),
        )
        .await;
    assert_eq!(sandbox.read_file("a.rs"), "alpha\nBETA\ngamma\n");
}

/// The worse half. An unmatched anchor fails loudly; a labelled *replacement*
/// is written into the source verbatim and the file stops compiling — ornith:35b
/// lost a run emitting `1088#f011\t    #[test]` as replacement text. Delete the
/// `relabelled_new` fallback and this goes red with the label in the file.
#[tokio::test]
async fn labels_in_the_replacement_are_never_written_into_the_file() {
    let sandbox = Sandbox::new();
    sandbox.write_file("b.rs", "alpha\nbeta\ngamma\n");
    // Read once so the tracker is fresh, then hand back a labelled replacement.
    let labelled = labelled_line(&sandbox, "b.rs", 3).await;

    sandbox
        .ok(
            "Edit",
            json!({ "file_path": "b.rs", "old_string": "beta", "new_string": labelled }),
        )
        .await;
    let after = sandbox.read_file("b.rs");
    assert!(!after.contains('#'), "a label reached the file: {after:?}");
    assert_eq!(after, "alpha\ngamma\ngamma\n");
}

// endregion: Read's labels inside the arguments
