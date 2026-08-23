//! `emma verify` — the decisions, without a model.
//!
//! The command spends money, so almost none of it may live in the part that
//! spends. Which rows are outstanding, what a reviewer is asked, what a verdict
//! means and what a receipt records are all plain functions here, and the model
//! is only the thing that fills in the middle.
//!
//! The exception is the fingerprint, which cannot be reasoned about at all: it
//! is a second implementation of a function whose history is a list of subtle
//! wrong answers, and the only useful test of it is agreement with the
//! authority against this repository.

use emma::verify::{brief, receipt, receipt_paths, rows_needing_review, verdict_of, Row, Verdict};
use serde_json::json;

/// A ledger with one row of each kind that matters.
fn ledger() -> serde_json::Value {
    json!({
        "requirements": [
            {
                "id": "DEF-001",
                "title": "a thing",
                "statement": "the thing holds",
                "implementation_evidence": ["a test exists"],
                "review_required": true,
                "review_evidence": []
            },
            {
                "id": "DEF-002",
                "title": "already reviewed",
                "statement": "also holds",
                "implementation_evidence": [],
                "review_required": true,
                "review_evidence": ["review-def-002.json"]
            },
            {
                "id": "DEF-003",
                "title": "review not required",
                "statement": "holds anyway",
                "implementation_evidence": [],
                "review_required": false,
                "review_evidence": []
            },
            {
                "id": "DEF-004",
                "title": "no review key at all",
                "statement": "holds",
                "implementation_evidence": []
            }
        ]
    })
}

/// Outstanding means "asks for a review and has none", and nothing else.
///
/// **A row already carrying a receipt must not be re-reviewed by accident.**
/// Each review is a billed model run, and a command that quietly redid finished
/// work would make `--limit` a lie about cost. The three negative cases below
/// are the ones that would each have cost money.
#[test]
fn only_rows_that_ask_for_a_review_and_lack_one_are_outstanding() {
    let rows = rows_needing_review(&ledger(), &[]).unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["DEF-001"],
        "a row that is reviewed, does not want review, or says nothing about it \
         was queued for a billed run"
    );
    assert_eq!(rows[0].evidence, vec!["a test exists".to_string()]);
}

/// Naming rows takes them whatever their evidence, and a typo is an error.
///
/// Re-reviewing a row is a legitimate request — a second opinion, or a review
/// against a tree that has moved. A misspelled id must not look like one: a run
/// that reviews nothing and a run that had nothing to do print the same thing
/// otherwise.
#[test]
fn a_named_row_is_taken_as_named_and_an_unknown_one_is_refused() {
    let rows = rows_needing_review(&ledger(), &["DEF-002".into()]).unwrap();
    assert_eq!(
        rows.len(),
        1,
        "a row named explicitly was skipped because it already had a receipt"
    );

    let err = rows_needing_review(&ledger(), &["DEF-001".into(), "DEF-999".into()])
        .expect_err("a row that does not exist was accepted");
    assert!(
        err.to_string().contains("DEF-999"),
        "the refusal does not name the id that was wrong: {err}"
    );
}

/// The verdict is the one the reviewer settled on, not the first it mentioned.
///
/// **A report that reasons out loud names every verdict on its way to one.**
/// Reading the first match turns a thought into a conclusion, which is a silent
/// wrong answer — and in this direction it would close a row a reviewer had
/// decided against.
#[test]
fn the_last_verdict_wins_and_a_missing_one_is_not_a_pass() {
    assert_eq!(verdict_of("VERDICT: UPHELD"), Some(Verdict::Upheld));
    assert_eq!(
        verdict_of("At first this looked fine.\nVERDICT: UPHELD\nBut then:\nVERDICT: OVERSTATED"),
        Some(Verdict::Overstated),
        "an earlier line was read as the conclusion"
    );
    // The spellings a model actually produces around a final line.
    assert_eq!(
        verdict_of("**VERDICT: REFUTED**"),
        Some(Verdict::Refuted),
        "a bolded verdict was not read"
    );
    assert_eq!(verdict_of("- VERDICT:  upheld  "), Some(Verdict::Upheld));

    // No verdict is not a failure and not a pass. It is an unusable report, and
    // the caller has to be able to tell the difference.
    assert_eq!(verdict_of("I ran out of context."), None);
    assert_eq!(
        verdict_of("VERDICT: maybe"),
        None,
        "a word that is not a verdict was read as one"
    );

    // Only UPHELD closes a row. OVERSTATED describes behaviour that may be
    // perfectly correct with evidence that does not defend it, and a gate
    // accepting those would be measuring paperwork.
    assert!(Verdict::Upheld.passes());
    assert!(!Verdict::Overstated.passes());
    assert!(!Verdict::Refuted.passes());
}

/// The brief asks for the things that have actually found defects here.
///
/// This is the whole product of the command: a reviewer told "check this row"
/// agrees with it. Asserted on the text because the text is the mechanism.
#[test]
fn the_brief_asks_a_reviewer_to_disprove_rather_than_confirm() {
    let row = Row {
        id: "DEF-007".into(),
        title: "the guard".into(),
        statement: "the guard holds".into(),
        evidence: vec!["a_test_that_defends_it".into()],
    };
    let text = brief(&row);

    assert!(text.contains("DEF-007") && text.contains("the guard holds"));
    assert!(
        text.contains("a_test_that_defends_it"),
        "the reviewer is not shown what it is meant to attack: {text}"
    );
    assert!(
        text.contains("find evidence that it is NOT"),
        "the brief does not ask for disproof, which is the only thing that has \
         found anything here: {text}"
    );
    assert!(
        text.contains("ONE LINE changed"),
        "the brief does not ask the question that finds a vacuous test: {text}"
    );
    assert!(
        text.contains("file:line"),
        "nothing asks the reviewer to cite, so a confident guess reads like a \
         finding: {text}"
    );
    assert!(
        text.contains("VERDICT: UPHELD | OVERSTATED | REFUTED"),
        "the reviewer is not told how to end, so its report cannot be parsed: {text}"
    );
    // Learned from the first live run, which spent its whole budget reading and
    // wrote sixty-nine bytes. A review that never reaches its verdict line costs
    // the same as one that does and is worth nothing.
    assert!(
        text.contains("BUDGET IS FINITE") && text.contains("STOP and write"),
        "nothing tells the reviewer to stop reading and answer: {text}"
    );

    // A row with no evidence still produces a usable brief, and says so rather
    // than leaving a blank where the claims should be.
    let bare = brief(&Row {
        id: "DEF-008".into(),
        title: "t".into(),
        statement: "s".into(),
        evidence: vec![],
    });
    assert!(
        bare.contains("states no evidence at all"),
        "an evidence-free row produced a brief with a hole in it: {bare}"
    );
}

/// A receipt records the verdict, the model that gave it, and where to read it.
///
/// **`pass` follows the verdict and never the run.** A reviewer that finished
/// cleanly and said REFUTED is a successful run and a failed row, and a receipt
/// that confused the two would close rows on the strength of the model having
/// replied.
#[test]
fn a_receipt_records_who_said_what_and_does_not_pass_on_a_refusal() {
    let r = receipt(
        "DEF-009",
        "claude-opus-5",
        Some(Verdict::Refuted),
        "verification/reviews/review-def-009-claude-opus-5.md",
        "abc123",
        "2026-08-23T12:00:00Z",
        "windows x86_64",
    );
    assert_eq!(r["kind"], "review");
    assert_eq!(r["requirement_ids"][0], "DEF-009");
    assert_eq!(
        r["producer"], "claude-opus-5",
        "the receipt does not name the model, so two reviews cannot be weighed \
         against each other"
    );
    assert_eq!(r["pass"], false);
    assert_eq!(r["tree_fingerprint"], "abc123");
    assert!(r["observed"].as_str().unwrap().contains("REFUTED"));

    assert_eq!(
        receipt("DEF-009", "m", Some(Verdict::Upheld), "p", "f", "t", "e")["pass"],
        true
    );

    // A reviewer that never produced a verdict is recorded as exactly that,
    // rather than smoothed into a failure: "said the row is wrong" and "did not
    // answer" are different things to whoever reads this next.
    let none = receipt("DEF-009", "m", None, "p", "f", "t", "e");
    assert_eq!(none["pass"], false);
    assert!(
        none["observed"].as_str().unwrap().contains("no verdict"),
        "an unusable report was recorded as though the reviewer had judged it"
    );
}

/// Receipt and report land beside their neighbours, under names a filesystem
/// accepts.
#[test]
fn a_model_id_with_punctuation_in_it_still_names_a_file() {
    let root = std::path::Path::new("/repo");
    let (receipt, report) = receipt_paths(root, "DEF-010", "anthropic/claude-opus-5:thinking");
    let receipt = receipt.to_string_lossy().replace('\\', "/");
    let report = report.to_string_lossy().replace('\\', "/");
    assert!(
        !receipt.contains(':') || receipt.starts_with("/repo"),
        "a model id with a colon in it produced a path Windows will not open: {receipt}"
    );
    assert!(receipt.contains("verification/receipts/review-def-010-"));
    assert!(report.contains("verification/reviews/review-def-010-"));
    assert!(receipt.ends_with(".json") && report.ends_with(".md"));
}

/// The Rust fingerprint agrees with the Python authority, on this repository.
///
/// **This is the only useful test of a reimplementation.** The authority's own
/// docstring is a list of subtle wrong answers it has already given -- hashing
/// HEAD's sha so that writing a receipt invalidated every receipt; hashing
/// HEAD-plus-diff so a receipt went stale at the moment it was committed;
/// missing a staged file entirely, which fingerprinted a tree with an extra
/// production module in it identically to a clean one. A second implementation
/// reasoned about rather than compared would be the next entry on that list.
///
/// It runs the real script against the real tree. If the two ever diverge this
/// is where it shows, and the answer is to change the Rust one: the Python is
/// the authority and carries the scar tissue.
///
/// **The skip is real and reported.** The script lives in a user-level skill
/// directory, so a checkout on another machine will not have it. Saying so is
/// the honest outcome; a test that silently passes when it could not run is the
/// false receipt this project keeps filing.
#[test]
fn the_rust_fingerprint_agrees_with_the_python_authority() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate")
        .to_path_buf();

    let script = match emma::verify::authority_script() {
        Some(s) => s,
        None => {
            eprintln!(
                "SKIPPED: the emma-parity verifier is not installed on this machine, so there \
                 is no authority to compare against"
            );
            return;
        }
    };

    let out = std::process::Command::new("python")
        .arg(&script)
        .arg("fingerprint")
        .current_dir(&repo)
        .output()
        .expect("running the authority");
    assert!(
        out.status.success(),
        "the authority itself failed, so this test proves nothing: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let authority = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        authority.len(),
        64,
        "the authority did not print a bare sha256, so the comparison below \
         would be against the wrong bytes: {authority:?}"
    );

    let ours = emma::verify::tree_fingerprint(&repo).expect("our own fingerprint");
    assert_eq!(
        ours, authority,
        "the Rust fingerprint has diverged from the authority. The Python one is \
         the authority and carries the reasoning; this one is what has to change."
    );
}
