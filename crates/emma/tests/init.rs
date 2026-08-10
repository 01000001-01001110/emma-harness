//! `emma init`, and the one property worth asserting about what it writes: the
//! harness it produces boots.
//!
//! Everything else here is about the two refusals. `init` exists because a
//! refusal with no next step is where somebody puts the tool down, so a test
//! that only checked the files appeared would miss the whole point of the
//! command — the message is the feature.

use std::path::Path;

use emma_harness::{Flavor, Harness};

/// `init` writes its report to a stream rather than to `println!` so a test can
/// read it. The report is the reason the command exists.
fn init(dir: &Path) -> Result<String, String> {
    let mut out = Vec::new();
    emma::commands::init(dir, &mut out)
        .map_err(|e| format!("{e:#}"))
        .map(|()| String::from_utf8(out).unwrap())
}

#[test]
fn what_init_writes_boots_as_a_harness() {
    // The claim `init` makes is not "some files exist". It is that a user who
    // ran it can now start Emma, so the assertion goes through the real
    // `Harness` loader and reads the values the loop would have been given.
    let dir = tempfile::tempdir().unwrap();
    let report = init(dir.path()).unwrap();

    // `load_selecting` rather than `load`: the latter reads `EMMA_PERSONA` from
    // the process environment, and a test whose result depends on the
    // developer's shell is a test that passes for the wrong reason.
    let harness = Harness::load_selecting(dir.path().join(".emma"), Flavor::Emma, None).unwrap();
    assert_eq!(harness.persona.as_deref(), Some("assistant"));
    assert!(
        !harness.is_empty(),
        "init wrote a harness with no instructions in it"
    );
    // Not "does it mention tools" any more. That sentence moved out of the
    // persona and into `goal::standing_contract`, because it is true of every
    // goal of every run and a fact the engine owns — the persona's job is to
    // say who this agent is and how it should work, which is the part a user
    // edits. The two asserted the same thing for a while, and the engine and
    // its configuration both claiming one rule is the drift this repository
    // keeps finding.
    assert!(
        harness.instructions.to_lowercase().contains("agent"),
        "the persona does not say what this agent is: {}",
        harness.instructions
    );
    assert!(
        emma::goal::standing_contract(&emma::goal::MarkerClaim).contains("tools"),
        "nothing tells the model it has tools"
    );

    // …and the report names what was written and what to type next.
    assert!(report.contains("config.json"), "{report}");
    assert!(report.contains("rules.md"), "{report}");
    assert!(
        report.contains("emma config check"),
        "the report ends on a wall of paths with no next step: {report}"
    );
}

#[test]
fn init_refuses_rather_than_merging_into_a_harness_that_exists() {
    // Merging configuration is how configuration stops being reason-about-able,
    // and this project has already ruled that way for `.emma/` against
    // `.claude/`. The refusal has to leave the existing file untouched.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".emma");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config.json"), "{\"default_persona\":null}").unwrap();

    let err = init(dir.path()).unwrap_err();
    assert!(err.contains(".emma"), "{err}");
    assert!(
        err.contains("already"),
        "the refusal does not say what exists: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("config.json")).unwrap(),
        "{\"default_persona\":null}",
        "init overwrote a harness it should have refused"
    );
    assert!(
        !root.join("personas").exists(),
        "init wrote half a harness beside the one it refused"
    );
}

#[test]
fn an_existing_claude_directory_is_named_and_emma_is_still_written() {
    // Emma already reads `.claude/`, so a user here may not need `init` at all —
    // and `.emma/` wins outright once it exists, which is a thing to be told
    // before it happens rather than after.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".claude")).unwrap();

    let report = init(dir.path()).unwrap();
    assert!(report.contains(".claude"), "{report}");
    assert!(
        report.contains("wins") || report.contains("instead of"),
        "the report does not say which one Emma will now read: {report}"
    );
    assert!(dir.path().join(".emma").join("config.json").is_file());
}
