//! `emma verify` — dispatch independent reviewers at the parity ledger.
//!
//! **The programme this serves is blocked on the one thing its author cannot
//! produce.** Every required row wants an independent review receipt, and the
//! implementer writing their own is the self-certification the whole scheme
//! exists to refuse. So the reviewer has to be somebody else: a fresh model
//! context, given the row and the tree and nothing else, briefed to find
//! evidence *against* the claim, and able to come back saying the row is
//! overstated. Nine defects were found that way in one day, none by the author.
//!
//! Everything here is a decision, and every decision is a plain function of its
//! inputs — which rows are outstanding, what the reviewer is asked, what a
//! verdict means, what a receipt records. `main.rs` supplies the model and the
//! tools. A scripted `Provider` can drive the whole command without a network,
//! which is the rule the agent loop is already built to.
//!
//! **What this cannot do, said before anyone assumes it.** A reviewer that only
//! reads the evidence the row already states is checking the argument, not the
//! tree. That is worth something and it is worth less than a reviewer that goes
//! and opens the file, so the brief asks for `file:line` and says an answer
//! without any is unusable. And a verdict is not a fact — it is one model's
//! reading, recorded with its name attached, which is why the receipt keeps the
//! whole report rather than only the word.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// region: Which rows are outstanding
// ---------------------------------------------------------------------------
// Which rows are outstanding
//
// The ledger is closed-world: rows are never deleted or downgraded to reach
// completion. So "what is left" is a question about evidence, not about
// membership, and it is answered the way the gate answers it.
// ---------------------------------------------------------------------------

/// One row, reduced to what a reviewer is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub statement: String,
    /// What the implementer claims, verbatim. **Handed over deliberately**: a
    /// reviewer that cannot see the claim can only re-derive it, and this
    /// programme's useful findings have all had the shape "that claim is true
    /// and the test it names does not defend it".
    pub evidence: Vec<String>,
}

/// Rows that still want an independent review, in ledger order.
///
/// `only` restricts to named ids when it is non-empty, and then takes them
/// whatever their evidence — re-reviewing a row is a legitimate thing to ask
/// for. An id that is not in the ledger is an error rather than a silent no-op,
/// because a typo that quietly reviews nothing looks exactly like a run that
/// had nothing to do.
pub fn rows_needing_review(ledger: &Value, only: &[String]) -> Result<Vec<Row>> {
    let requirements = ledger
        .get("requirements")
        .and_then(Value::as_array)
        .context("the ledger has no `requirements` array")?;

    let wanted: BTreeSet<&str> = only.iter().map(String::as_str).collect();
    if !wanted.is_empty() {
        let known: BTreeSet<&str> = requirements
            .iter()
            .filter_map(|r| r.get("id").and_then(Value::as_str))
            .collect();
        let missing: Vec<&str> = wanted.difference(&known).copied().collect();
        if !missing.is_empty() {
            bail!("no such row(s) in the ledger: {}", missing.join(", "));
        }
    }

    let mut out = Vec::new();
    for r in requirements {
        let id = r.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        if !wanted.is_empty() {
            if !wanted.contains(id) {
                continue;
            }
        } else {
            // An absent key and an empty array mean the same thing, and a row
            // that does not ask for review is not outstanding.
            let required = r
                .get("review_required")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let have = r
                .get("review_evidence")
                .and_then(Value::as_array)
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            if !required || have {
                continue;
            }
        }
        out.push(Row {
            id: id.to_string(),
            title: r
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            statement: r
                .get("statement")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            evidence: r
                .get("implementation_evidence")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    Ok(out)
}

// endregion: Which rows are outstanding

// region: What the reviewer is asked
// ---------------------------------------------------------------------------
// What the reviewer is asked
//
// The brief is the whole product. A reviewer told "check this row" agrees with
// it; the ones that found real defects here were told to assume the author
// believes the work is finished, and to find evidence that prevents completion.
// ---------------------------------------------------------------------------

/// The verdict a reviewer must end on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The claim holds and the evidence named really defends it.
    Upheld,
    /// The claim is true and the evidence named does not defend it — the
    /// commonest useful finding, and the one this programme keeps paying for.
    Overstated,
    /// The claim itself is wrong.
    Refuted,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upheld => "UPHELD",
            Self::Overstated => "OVERSTATED",
            Self::Refuted => "REFUTED",
        }
    }

    /// Whether this verdict lets the row close.
    ///
    /// Only `UPHELD` does. **`OVERSTATED` deliberately does not**, even though
    /// the behaviour it describes may be perfectly correct: a row whose evidence
    /// does not defend it is a false receipt, and a gate that accepted those
    /// would be measuring paperwork.
    pub fn passes(self) -> bool {
        matches!(self, Self::Upheld)
    }
}

/// The reviewer's verdict, read off its report.
///
/// **The last occurrence wins, not the first.** A report that reasons out loud
/// will name several verdicts while weighing them — "this looks OVERSTATED,
/// but…" — and the one that counts is the one it settled on. Taking the first
/// match reads a thought as a conclusion, which is a silent wrong answer of
/// exactly the kind this project is built against.
///
/// `None` when no verdict line is present. That is neither a pass nor a
/// failure: it is an unusable report, and the caller says so rather than
/// guessing which way the reviewer was leaning.
pub fn verdict_of(report: &str) -> Option<Verdict> {
    let mut found = None;
    for line in report.lines() {
        let line = line.trim().trim_start_matches(['*', '#', '-', ' ']);
        let Some(rest) = line.strip_prefix("VERDICT:") else {
            continue;
        };
        let word = rest
            .trim()
            .trim_matches(['*', '`', ' '])
            .to_ascii_uppercase();
        found = match word.as_str() {
            "UPHELD" => Some(Verdict::Upheld),
            "OVERSTATED" => Some(Verdict::Overstated),
            "REFUTED" => Some(Verdict::Refuted),
            // A line that says VERDICT and then something else leaves the last
            // real answer standing rather than erasing it.
            _ => found,
        };
    }
    found
}

/// What one reviewer is asked about one row.
pub fn brief(row: &Row) -> String {
    let evidence = if row.evidence.is_empty() {
        "  (the row states no evidence at all, which is itself a finding)".to_string()
    } else {
        row.evidence
            .iter()
            .map(|e| format!("  - {e}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "You are an independent reviewer of one row in a verification ledger for a Rust CLI \
         agent. Assume the implementer believes this row is finished and correct. Your job is to \
         find evidence that it is NOT. Do not confirm success.\n\
         \n\
         Use your tools to READ THE CODE AND THE TESTS. A review that only reasons about the \
         claims below is worth little; every finding that mattered in this project came from \
         opening the file. You cannot edit anything and must not try.\n\
         \n\
         ROW {id}: {title}\n\
         \n\
         WHAT THE ROW CLAIMS:\n\
         {statement}\n\
         \n\
         WHAT THE IMPLEMENTER OFFERS AS EVIDENCE:\n\
         {evidence}\n\
         \n\
         ATTACK IT ON THESE, in the order they have actually mattered here:\n\
         1. Does a named test defend the guarantee? Take each test the row names and ask what ONE \
         LINE changed in the PRODUCTION code would leave it green. If you find one, the row is \
         overstated, and that is the most valuable answer you can give.\n\
         2. Is the guarantee delivered? A decision tested in a library whose single call site has \
         no test is a defect this project has hit six times: delete the call site, the suite stays \
         green, and the user is silently never told.\n\
         3. Can the assertion fail at all? An assertion satisfied by the ordinary state of the \
         world - an absent file that nothing creates, a digit in output full of hex - proves \
         nothing.\n\
         4. Does the claim overstate what was checked? Certified means run against the real thing. \
         Fixtures agree with their author.\n\
         5. Is anything silent - a failure, a truncation, a skipped file, a plausible success?\n\
         \n\
         Quote file:line for every claim. If you could not determine something, say so plainly: an \
         honest 'I could not check this' is worth more than a guess, and a confident wrong finding \
         costs a day. Report negative results too - what you checked and found solid is part of \
         the receipt.\n\
         \n\
         YOUR BUDGET IS FINITE AND READING WILL EXHAUST IT. The first review ever run here spent \
         its entire budget opening files and stopped before writing anything, which cost real \
         money and produced no receipt at all. Read what you need and then STOP and write. A \
         short report ending in a verdict is worth more than a thorough investigation nobody \
         can read. Aim for well under a page, and write the verdict line even if you are less \
         certain than you would like - say so in the report instead.\n\
         \n\
         End your report with exactly one line, on its own:\n\
         VERDICT: UPHELD | OVERSTATED | REFUTED\n\
         \n\
         UPHELD - the claim holds AND the evidence named really defends it.\n\
         OVERSTATED - the claim is true but the evidence named does not defend it.\n\
         REFUTED - the claim itself is wrong.",
        id = row.id,
        title = row.title,
        statement = row.statement,
        evidence = evidence,
    )
}

// endregion: What the reviewer is asked

// region: The receipt
// ---------------------------------------------------------------------------
// The receipt
//
// A receipt is the artefact, not the verdict word. The whole report is kept
// beside it, because a reviewer's reasoning is what a later reader needs in
// order to disagree with it — and disagreeing with a review has already been
// necessary here more than once.
// ---------------------------------------------------------------------------

/// Where a row's receipt and report are written: `(receipt, report)`.
pub fn receipt_paths(root: &Path, id: &str, model: &str) -> (PathBuf, PathBuf) {
    let slug = slug(id, model);
    (
        root.join("verification")
            .join("receipts")
            .join(format!("{slug}.json")),
        root.join("verification")
            .join("reviews")
            .join(format!("{slug}.md")),
    )
}

/// A file name that survives a model id containing `/`, `.` or `:`.
fn slug(id: &str, model: &str) -> String {
    let model: String = model
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("review-{}-{}", id.to_ascii_lowercase(), model)
}

/// The receipt for one review.
pub fn receipt(
    id: &str,
    model: &str,
    verdict: Option<Verdict>,
    report_path: &str,
    fingerprint: &str,
    when: &str,
    environment: &str,
) -> Value {
    json!({
        "id": slug(id, model),
        "kind": "review",
        "requirement_ids": [id],
        // The model, named. A receipt whose producer is "an independent
        // reviewer" cannot be weighed against another one later.
        "producer": model,
        "command": format!("emma verify --rows {id} --model {model}"),
        "observed": match verdict {
            Some(v) => format!("VERDICT: {}", v.as_str()),
            // Recorded as the fact it is rather than smoothed into a failure:
            // "the reviewer said this row is wrong" and "the reviewer did not
            // answer" are different things to whoever reads this next.
            None => "no verdict line in the report".to_string(),
        },
        "pass": verdict.map(Verdict::passes).unwrap_or(false),
        "tree_fingerprint": fingerprint,
        "evidence_files": [report_path],
        "environment": environment,
        "timestamp": when,
    })
}

// endregion: The receipt

// region: The tree fingerprint
// ---------------------------------------------------------------------------
// The tree fingerprint
//
// A second implementation of a function with a long and expensive history, and
// therefore the one piece here that has to be certified rather than believed.
// `the_rust_fingerprint_agrees_with_the_python_authority` runs both against this
// repository and compares; if they ever diverge, that is where it shows.
// ---------------------------------------------------------------------------

/// Paths under these prefixes are not production and never move the value.
///
/// Identical to the verifier's list, and identical for the reason recorded
/// there: a commit writing the very receipts a fingerprint date-stamps must not
/// invalidate every receipt on the spot.
const EXCLUDED: &[&str] = &["verification/", ".claude/skills/emma-parity/"];

/// Where the Python authority lives, when this machine has it.
///
/// **The authority is a user-level skill, not part of this repository**, so a
/// checkout elsewhere will not have it and the certification test says so
/// rather than passing in silence. Two spellings because Windows and everything
/// else disagree about where a home directory is, and neither is worth a
/// dependency.
pub fn authority_script() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let path = Path::new(&home)
        .join(".claude")
        .join("skills")
        .join("emma-parity")
        .join("scripts")
        .join("verify_completion.py");
    path.is_file().then_some(path)
}

/// Fingerprint the production tree **on disk** — HEAD, the index and the
/// worktree.
///
/// All three lists matter and each was learned the hard way, per the authority's
/// own notes: `ls-tree HEAD` misses a newly added file, `ls-files --others`
/// means "not in the index" and misses it too, and a staged file appearing in
/// neither once fingerprinted identically to a clean tree. `-z` on every call,
/// so there is no quoting to reason about.
///
/// A path that is not a file on disk drops out rather than hashing as absent:
/// recording absence reintroduces the asymmetry where a deletion has one value
/// before its commit and another after.
pub fn tree_fingerprint(root: &Path) -> Result<String> {
    let paths_from = |args: &[&str]| -> Result<Vec<String>> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect())
    };

    let mut wanted: BTreeSet<String> = BTreeSet::new();
    for args in [
        &["ls-tree", "-r", "--name-only", "-z", "HEAD"][..],
        &["ls-files", "-z", "--others", "--exclude-standard"][..],
        &["ls-files", "-z", "--cached"][..],
    ] {
        for p in paths_from(args)? {
            if !EXCLUDED.iter().any(|e| p.starts_with(e)) {
                wanted.insert(p);
            }
        }
    }

    let mut h = Sha256::new();
    for rel in &wanted {
        let file = root.join(rel);
        if !file.is_file() {
            continue;
        }
        let bytes = std::fs::read(&file).with_context(|| format!("reading {rel}"))?;
        h.update(rel.as_bytes());
        h.update(b"\0");
        h.update(Sha256::digest(&bytes));
        h.update(b"\0");
    }
    Ok(format!("{:x}", h.finalize()))
}

// endregion: The tree fingerprint
