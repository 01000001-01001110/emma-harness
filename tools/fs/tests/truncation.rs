//! What every tool here promises when it cuts something.
//!
//! The defect these were written after is not a crash and not a missing
//! message. `Glob` returned 1000 of 64,097 paths, *printed both numbers to the
//! terminal*, and then set a bare flag — so the model was told "something was
//! cut" with no cap named, no amount lost, and no argument to reach for. The
//! terminal fell back to "output was truncated by the tool, which did not say
//! by which limit", which is how anyone found out.
//!
//! So the guarantee is one sentence in three parts, and each part is asserted
//! separately below because they fail separately:
//!
//! 1. A cut result **says it was cut** — the flag, and the note in `content`,
//!    because the model reads content and not the struct.
//! 2. It **names the cap and the loss** in `truncation`, which the runtime
//!    quotes verbatim to both audiences.
//! 3. Its **remedy is one that works**. The rule was paid for in the web tools:
//!    "narrow the request" was appended to a cut *link inventory*, where
//!    narrowing cannot produce more links. Here the mirror-image mistake is
//!    advertising a size argument that does not exist, so the tests check for
//!    the absence of invented knobs as hard as for the presence of real ones.
//!
//! The first test is the cheap net over the whole class; the rest are the
//! per-tool sentences, because a generic assertion cannot tell whether a
//! `Glob` notice is about paths or about a walk.

mod support;

use serde_json::json;
use support::Sandbox;

/// A cut with no stated reason is exactly the defect, so this walks every tool
/// that can be made to cut and demands the same three things of each. It is the
/// test that notices a *new* tool with the old habit: `ToolOutcome::truncated()`
/// still exists and still compiles, and this is what makes choosing it visible.
#[tokio::test]
async fn no_tool_here_cuts_without_naming_the_cap() {
    let sandbox = Sandbox::new();

    // Glob: more matches than the result cap.
    for n in 0..1100 {
        sandbox.write_file(&format!("many/f{n}.txt"), "x");
    }
    // Grep: more matching lines than the output cap.
    let many: String = (0..600).map(|n| format!("needle {n}\n")).collect();
    sandbox.write_file("hits.txt", &many);
    // Bash: four times the per-stream cap, so a regressed cap could not pass.
    let line = "0123456789012345678901234567890123456789012345678901234567890123\n";
    sandbox.write_file("noisy.txt", &line.repeat(256 * 1024 / line.len()));
    // Read: more lines than the line cap.
    let long: String = (1..=2500).map(|n| format!("line {n}\n")).collect();
    sandbox.write_file("long.txt", &long);

    let cases = [
        ("Glob", json!({ "pattern": "many/*.txt" })),
        ("Grep", json!({ "pattern": "needle", "path": "hits.txt" })),
        ("Read", json!({ "file_path": "long.txt" })),
        (
            "Bash",
            json!({ "command": "cat noisy.txt", "timeout_ms": 300000 }),
        ),
    ];

    for (tool, args) in cases {
        let outcome = sandbox.ok(tool, args.clone()).await;
        assert!(outcome.truncated, "{tool} cut its output and hid it");
        let reason = outcome
            .truncation
            .as_deref()
            .unwrap_or_else(|| panic!("{tool} set the flag and named no limit: {outcome:?}"));
        assert!(reason.len() > 40, "{tool} said nothing usable: {reason:?}");
        // Every notice must contain a number: which cap, and how much of what
        // there was. A sentence with no number is the fallback wording wearing
        // a different coat.
        assert!(
            reason.chars().any(|c| c.is_ascii_digit()),
            "{tool} named no cap: {reason:?}"
        );
        // And the model reads `content`, not the struct. Both, deliberately:
        // a caller that renders only one of them still tells the truth.
        assert!(
            outcome.content.contains("[truncated:"),
            "{tool} told the runtime and not the model: {}",
            &outcome.content[outcome.content.len().saturating_sub(200)..]
        );
    }
}

/// The reported case, reproduced small. Every number in it is one the tool
/// already knew and threw away: it printed "1000 of 1100" to the terminal in
/// the same call that told the model only "truncated".
#[tokio::test]
async fn glob_says_which_thousand_and_offers_the_only_remedy_there_is() {
    let sandbox = Sandbox::new();
    for n in 0..1100 {
        sandbox.write_file(&format!("many/f{n}.txt"), "x");
    }

    let outcome = sandbox.ok("Glob", json!({ "pattern": "many/*.txt" })).await;
    let reason = outcome.truncation.expect("Glob cut 100 paths silently");

    assert!(reason.contains("1000 of 1100"), "{reason}");
    assert!(reason.contains("100 dropped"), "{reason}");
    // Which thousand — the cap is on a sorted list, so the answer is a sample
    // and not an arbitrary slice, and the model cannot know that unless told.
    assert!(reason.contains("most recently modified"), "{reason}");
    assert!(reason.contains("no argument raises"), "{reason}");
    assert!(reason.contains("`pattern`"), "no remedy: {reason}");
    // There is no size knob and there is deliberately not going to be one; see
    // `MAX_RESULTS`. Naming one would cost a turn and return a bigger useless
    // answer.
    assert!(!reason.contains("max_results"), "invented a knob: {reason}");
    assert_eq!(
        outcome.display.as_deref(),
        Some("many/*.txt: 1000 of 1100 paths")
    );
}

/// `head_limit` exists, and it cannot raise the ceiling — it only lowers it.
/// That is exactly the shape of the `WebFetch` bug (an argument the model was
/// told about that would not have helped), so the notice has to distinguish the
/// two cases rather than name `head_limit` and stop.
#[tokio::test]
async fn grep_says_whether_head_limit_can_help() {
    let sandbox = Sandbox::new();
    let many: String = (0..600).map(|n| format!("needle {n}\n")).collect();
    sandbox.write_file("hits.txt", &many);

    let ceiling = sandbox
        .ok("Grep", json!({ "pattern": "needle", "path": "hits.txt" }))
        .await;
    let reason = ceiling.truncation.expect("Grep cut 100 matches silently");
    assert!(reason.contains("500 of 600 matches"), "{reason}");
    assert!(
        reason.contains("`head_limit` can lower but not raise"),
        "a model told only 'head_limit' would re-run and get the same 500: {reason}"
    );
    assert!(reason.contains("output_mode=count"), "no remedy: {reason}");

    // The other half: a caller who set the limit itself is told it has room,
    // and told where the room stops.
    let asked = sandbox
        .ok(
            "Grep",
            json!({ "pattern": "needle", "path": "hits.txt", "head_limit": 10 }),
        )
        .await;
    let reason = asked.truncation.expect("Grep cut at head_limit silently");
    assert!(reason.contains("head_limit=10"), "{reason}");
    assert!(reason.contains("500-line ceiling"), "{reason}");
}

/// A file nobody opened cannot be reported as a file with no matches. This is
/// the half of `Grep` that is not about output at all: the answer "no matches"
/// is a claim about every candidate file, and the size cap makes it false
/// without anything else in the result changing.
#[tokio::test]
async fn grep_admits_the_files_it_never_opened() {
    let sandbox = Sandbox::new();
    // Just over the 8 MiB per-file limit, and holding the pattern.
    let mut huge = "x".repeat(8 * 1024 * 1024);
    huge.push_str("\nneedle\n");
    sandbox.write_file("huge.log", &huge);

    let outcome = sandbox.ok("Grep", json!({ "pattern": "needle" })).await;
    assert!(
        outcome.truncated,
        "'no matches' was reported over a file that was never read: {outcome:?}"
    );
    let reason = outcome.truncation.clone().expect("silent skip");
    assert!(reason.contains("1 text files were not opened"), "{reason}");
    assert!(reason.contains("Read"), "no remedy: {reason}");
    // The empty result still carries the reason in content — this is the branch
    // where the model has nothing else to read.
    assert!(outcome.content.contains("[truncated:"), "{outcome:?}");

    // The negative control, and the reason the skip is counted by *kind*: a
    // tree with a PNG in it must not report every search as incomplete. A
    // binary file is a file with no lines, and warning about it on every call
    // is how a warning stops being read.
    let plain = Sandbox::new();
    plain.write_file("code.rs", "let needle = 1;\n");
    std::fs::write(plain.root().join("icon.png"), [0x89, b'P', 0xff, 0xfe]).expect("write");
    let outcome = plain.ok("Grep", json!({ "pattern": "needle" })).await;
    assert!(
        !outcome.truncated,
        "a binary file made a whole search look cut: {outcome:?}"
    );

    // And the case the first live run found: a *large* binary. Before the
    // prefix sniff, a repo-wide Grep in this project reported 499 files not
    // opened, every one of them an .rlib or a .pdb under `target/`. That notice
    // was true, useless, and on almost every search — which is how a warning
    // stops being read. Size alone cannot tell a log from an object file.
    let objects = Sandbox::new();
    objects.write_file("code.rs", "let needle = 1;\n");
    let mut blob = vec![0u8; 9 * 1024 * 1024];
    blob[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    std::fs::write(objects.root().join("big.rlib"), &blob).expect("write");
    let outcome = objects.ok("Grep", json!({ "pattern": "needle" })).await;
    assert!(
        !outcome.truncated,
        "a 9 MiB object file was reported as an unsearched text file: {outcome:?}"
    );
}

/// `Read` already listed its reasons in `content` and then handed the runtime a
/// bare flag, so the terminal printed the fallback line beside a result that
/// knew perfectly well which of its three caps had fired. Same sentence, both
/// places.
#[tokio::test]
async fn read_hands_the_same_sentence_to_both_audiences() {
    let sandbox = Sandbox::new();
    let long: String = (1..=2500).map(|n| format!("line {n}\n")).collect();
    sandbox.write_file("long.txt", &long);

    let outcome = sandbox.ok("Read", json!({ "file_path": "long.txt" })).await;
    let reason = outcome.truncation.clone().expect("Read named no cap");
    assert!(reason.contains("lines 1-2000 of 2500"), "{reason}");
    assert!(reason.contains("continue with offset 2001"), "{reason}");
    assert!(reason.contains("`limit` cannot raise"), "{reason}");
    assert!(
        outcome.content.contains(&reason),
        "the content and the notice tell different stories: {reason}"
    );
}

/// Two failures wearing one flag: the 64 KiB cap, and the case where the drain
/// race is lost and the body is empty rather than a prefix. Only the first is
/// tested here — see the module note in `bash.rs` for why the second needs a
/// grandchild holding the pipe open, which no fixture can arrange reliably.
#[tokio::test]
async fn bash_names_the_stream_cap_and_a_remedy_that_is_not_a_bigger_cap() {
    let sandbox = Sandbox::new();
    let line = "0123456789012345678901234567890123456789012345678901234567890123\n";
    sandbox.write_file("noisy.txt", &line.repeat(256 * 1024 / line.len()));
    let outcome = sandbox
        .ok(
            "Bash",
            json!({ "command": "cat noisy.txt", "timeout_ms": 300000 }),
        )
        .await;
    let reason = outcome.truncation.expect("Bash cut output silently");
    assert!(reason.contains("65536-byte per-stream cap"), "{reason}");
    assert!(
        reason.contains("no \nargument raises") || reason.contains("no argument raises"),
        "{reason}"
    );
    // Redirecting to a file is the remedy that exists. `timeout_ms` is the only
    // argument `Bash` has and it does nothing about output volume, so a notice
    // pointing at it would be the "narrow the request" mistake again.
    assert!(reason.contains("file"), "no remedy: {reason}");
    assert!(!reason.contains("timeout_ms"), "wrong knob: {reason}");
}

/// The other side of every assertion above: a result that was *not* cut must
/// carry neither the flag nor a reason. Without this the whole file is
/// satisfiable by marking everything truncated, which would be worse than the
/// defect it replaces — a warning on every result is a warning nobody reads.
#[tokio::test]
async fn a_whole_answer_claims_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write_file("small.txt", "one\ntwo\n");

    for (tool, args) in [
        ("Read", json!({ "file_path": "small.txt" })),
        ("Glob", json!({ "pattern": "*.txt" })),
        ("Grep", json!({ "pattern": "one" })),
        ("Bash", json!({ "command": "echo hello" })),
    ] {
        let outcome = sandbox.ok(tool, args).await;
        assert!(
            !outcome.truncated,
            "{tool} claimed a cut that never happened"
        );
        assert!(outcome.truncation.is_none(), "{tool}: {outcome:?}");
        assert!(
            !outcome.content.contains("[truncated"),
            "{tool}: {outcome:?}"
        );
    }
}
