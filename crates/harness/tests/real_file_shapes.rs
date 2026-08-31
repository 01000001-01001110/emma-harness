//! The six configuration surfaces, read as files a real machine actually
//! contains rather than as files a test author typed.
//!
//! **The reason this file exists is a receipt.** A frontmatter parser here once
//! dropped 86 of 90 real agent files, and later 101 of 323 real skills, because
//! every fixture it was tested against ended its lines with `\n` and every file
//! on the machine ended them with `\r\n`. Nothing failed. The catalogue was just
//! short, and no test could tell. Fixtures agree with their author.
//!
//! So each test below picks one surface and one shape a real file has and a
//! fixture usually does not — a byte-order mark, CRLF, a file that is not UTF-8
//! at all, a directory one level deeper than the loader looks — and asserts what
//! the *caller* can see: what is in the catalogue, what the notes say, whether
//! the boot failed.
//!
//! **Every report here has a control asserting silence.** A note that fires on
//! an ordinary directory is the same as no note, because the operator stops
//! reading them — and the ordinary case is the one that runs every day.

mod support;

use emma_harness::{Flavor, Harness};
use std::path::Path;
use support::*;

/// Write bytes rather than a `&str`, because the point of several tests below
/// is a file that is not valid UTF-8 and so cannot be written any other way.
fn write_bytes(path: &Path, body: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

// region: settings.json and the spine — the byte-order mark
// ---------------------------------------------------------------------------
// settings.json and the spine
//
// One byte, three files, and the failure is a refusal to start rather than a
// silent drop — which makes it louder than the frontmatter defect and no less
// wrong, because the file it blames is correct.
// ---------------------------------------------------------------------------

/// If this fails: a Windows operator whose `settings.json` came out of a
/// PowerShell redirect cannot start Emma at all, and the error tells them their
/// JSON is malformed at column 1 when it is not.
///
/// `>`, `Out-File` and `Set-Content -Encoding utf8` all write a UTF-8
/// byte-order mark on this platform. `serde_json` refuses it. The mark was
/// already stripped for markdown frontmatter — for exactly this reason, after
/// exactly this class of miss — and the JSON readers never got it.
#[test]
fn a_settings_json_written_with_a_byte_order_mark_still_boots() {
    let base = scratch("bom-settings");
    let root = base.join(".claude");
    write(
        &root.join("settings.json"),
        "\u{feff}{\"permissions\":{\"deny\":[\"Bash(rm:*)\"]}}",
    );
    let h = Harness::load(&root).expect("a BOM must not refuse the boot");
    // Not merely that it booted: the block behind the mark has to have been
    // read. A loader that skipped the file entirely would also "not fail".
    assert_eq!(
        h.permissions()
            .iter()
            .map(|p| p.rule.as_str())
            .collect::<Vec<_>>(),
        vec!["Bash(rm:*)"],
        "the settings block behind the byte-order mark was not read"
    );
}

/// If this fails: the same operator cannot start Emma from a `.emma/` project
/// either, and `config.json` is the file that carries the persona — so the
/// failure is total rather than partial.
#[test]
fn an_emma_config_written_with_a_byte_order_mark_still_boots() {
    let base = scratch("bom-config");
    let root = base.join(".emma");
    write(
        &root.join("config.json"),
        "\u{feff}{\"default_persona\":\"assistant\",\"personas\":{\"assistant\":{}}}",
    );
    write(&root.join("personas/assistant/rules.md"), "Be exact.");
    let h = Harness::load(&root).expect("a BOM must not refuse the boot");
    assert_eq!(
        h.persona.as_deref(),
        Some("assistant"),
        "the spine behind the byte-order mark was not read"
    );
    assert_eq!(h.instructions, "Be exact.");
}

/// If this fails: standing grants an operator accumulated in
/// `settings.local.json` stop the session from starting instead of applying —
/// and this is the one file in the tree Emma itself writes back to, so a mark
/// arriving from an editor round-trip breaks the file Emma owns.
#[test]
fn a_local_settings_file_written_with_a_byte_order_mark_still_boots() {
    let base = scratch("bom-local");
    let root = base.join(".claude");
    write(
        &root.join("settings.local.json"),
        "\u{feff}{\"permissions\":{\"allow\":[\"Bash(git status:*)\"]}}",
    );
    let h = Harness::load(&root).expect("a BOM must not refuse the boot");
    assert_eq!(
        h.permissions()
            .iter()
            .map(|p| p.rule.as_str())
            .collect::<Vec<_>>(),
        vec!["Bash(git status:*)"],
        "the remembered grant behind the byte-order mark was lost"
    );
}

/// The control, and the reason the three above are not just "does it parse".
///
/// If this fails: genuinely malformed JSON has started booting, and an operator
/// runs with a `deny` list that was never read. The BOM fix must not have
/// widened into tolerating anything else.
#[test]
fn json_that_is_actually_malformed_still_refuses_the_boot() {
    let base = scratch("bom-control");
    let root = base.join(".claude");
    write(
        &root.join("settings.local.json"),
        "\u{feff}{\"permissions\":{\"allow\":[\"Bash(ls:*)\",]}}",
    );
    let err = Harness::load(&root).expect_err("a trailing comma is still malformed");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("settings.local.json") && msg.contains("is malformed"),
        "the refusal must name the file and the problem: {msg}"
    );
}

/// The user scope reads a file belonging to another program, so it notes rather
/// than refuses — but the note has to stop firing once the file is readable.
///
/// If this fails: the operator's global `deny` rules are silently absent while
/// Emma reports nothing wrong, which is the exact case `user_permissions` was
/// given a note for.
#[test]
fn a_user_settings_file_with_a_byte_order_mark_yields_its_deny_rules_and_no_note() {
    let home = scratch("bom-user");
    write(
        &home.join(".claude/settings.json"),
        "\u{feff}{\"permissions\":{\"deny\":[\"Bash(curl:*)\"],\"allow\":[\"Bash(rm:*)\"]}}",
    );
    let (entries, notes) = emma_harness::user_permissions(Some(&home)).expect("no failure");
    assert_eq!(
        entries.iter().map(|e| e.rule.as_str()).collect::<Vec<_>>(),
        vec!["Bash(curl:*)"],
        "the global deny rule behind the mark is not in force"
    );
    assert!(
        notes.is_empty(),
        "a readable file must not be reported as unreadable: {notes:?}"
    );
}

// endregion: settings.json and the spine — the byte-order mark

// region: skills/ — a file that is not UTF-8, and a directory one level too deep
// ---------------------------------------------------------------------------
// skills/
//
// Two shapes, both of which produced an answer nobody wrote down: one took the
// boot with it against the loader's own documented ruling, and the other loaded
// an empty catalogue in silence.
// ---------------------------------------------------------------------------

/// If this fails: one `SKILL.md` saved in Latin-1 anywhere in a `.claude/`
/// library stops Emma from starting, and every other skill in the directory is
/// lost with it.
///
/// `ClaudeFront`'s doc has said since it was written that "a file that cannot be
/// read at all is skipped with a warning rather than taken as a reason not to
/// start". That covered a *parse* failure; the read was `?`. The agent loader
/// one file away already noted the identical input and carried on.
#[test]
fn a_skill_that_is_not_utf8_costs_that_skill_and_not_the_session() {
    let base = scratch("skill-latin1");
    let root = base.join(".claude");
    with_skill(&root, "good", "Still here.", "body");
    // 0xE9 is `é` in Latin-1 and is not valid UTF-8 on its own.
    write_bytes(
        &root.join("skills/latin/SKILL.md"),
        b"---\nname: latin\ndescription: caf\xe9 rules\n---\nbody\n",
    );
    let h = Harness::load(&root).expect("one unreadable skill must not cost the harness");
    assert_eq!(
        h.skill_names(),
        vec!["good"],
        "the readable skill beside the unreadable one still has to load"
    );
    let notes = h.skill_notes().join("\n");
    assert!(
        notes.contains("could not be read"),
        "the skill vanished without a word: {notes}"
    );
    assert!(
        notes.contains("1 skill(s)"),
        "an unreadable skill has to count towards the shortfall like a bad parse does: {notes}"
    );
}

/// And `.emma/` stays fatal, which is what the tolerance above costs.
///
/// If this fails: a file the operator wrote for Emma, in Emma's own directory,
/// is quietly not in the catalogue — the line this crate draws everywhere else
/// between somebody else's format and its own has moved.
#[test]
fn an_emma_skill_that_is_not_utf8_still_refuses_the_boot() {
    let root = one_persona("skill-latin1-emma", "rules");
    write_bytes(
        &root.join("skills/latin/SKILL.md"),
        b"---\nname: latin\ndescription: caf\xe9 rules\n---\nbody\n",
    );
    let err = Harness::load(&root).expect_err("Emma's own format is strict");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("SKILL.md"),
        "the refusal must name the file: {msg}"
    );
}

/// If this fails: a skill library laid out one directory deeper — which is how
/// a plugin ships its skills — loads as an empty catalogue and nothing says so.
/// The model then cannot find a skill the operator can see on disk.
///
/// The sibling command loader had grown exactly this count for exactly this
/// reason; skills never got it.
#[test]
fn skills_one_directory_too_deep_are_counted_rather_than_silently_dropped() {
    let base = scratch("skill-nested");
    let root = base.join(".claude");
    with_skill(&root, "top", "At the level the loader reads.", "body");
    write(
        &root.join("skills/plugin/buried/SKILL.md"),
        "---\nname: buried\ndescription: One level too deep.\n---\nbody\n",
    );
    write(
        &root.join("skills/plugin/alsoburied/SKILL.md"),
        "---\nname: alsoburied\ndescription: Also too deep.\n---\nbody\n",
    );
    let h = Harness::load(&root).expect("nesting must not stop the boot");
    assert_eq!(
        h.skill_names(),
        vec!["top"],
        "only `skills/<name>/SKILL.md` is loaded"
    );
    let notes = h.skill_notes().join("\n");
    assert!(
        notes.contains("2 skill(s) below"),
        "the buried skills were dropped without a count: {notes}"
    );
    assert!(
        notes.contains("one directory too deep"),
        "the note has to say what is wrong with them, not merely that there are some: {notes}"
    );
}

/// The control for both of the above.
///
/// If this fails: every ordinary load reports a problem it does not have, and
/// an operator who reads the notes twice stops reading them — at which point the
/// two reports above are worth nothing.
#[test]
fn an_ordinary_skills_directory_reports_nothing_at_all() {
    let base = scratch("skill-quiet");
    let root = base.join(".claude");
    with_skill(&root, "adr", "Write an ADR.", "body");
    with_skill(&root, "debug", "Debug a failure.", "body");
    // A subdirectory with no `SKILL.md` and no children is the shape a stale
    // checkout leaves behind; it is not a buried skill and must not be counted
    // as one.
    std::fs::create_dir_all(root.join("skills/leftover")).expect("mkdir");
    let h = Harness::load(&root).expect("load");
    assert_eq!(h.skill_names(), vec!["adr", "debug"]);
    assert!(
        h.skill_notes().is_empty(),
        "a directory with nothing wrong with it produced a report: {:?}",
        h.skill_notes()
    );
}

// endregion: skills/

// region: agents/ — the shape the whole lesson came from
// ---------------------------------------------------------------------------
// agents/
//
// 86 of 90 real files were dropped here, by a byte-exact `---\n` opener on a
// machine whose editor writes `---\r\n`. The fix landed; the fixture that would
// have caught it did not — every agent test in this crate is written with unix
// line endings, so the guarantee is undefended in the one loader it was paid
// for in.
// ---------------------------------------------------------------------------

/// If this fails: an agent file written on Windows silently means nothing it
/// says. Its `description` is invisible, so it is never offered for delegation;
/// its `tools` list does not apply; and its `name:`, `model:` and `tools:` lines
/// are handed to the model as standing instructions instead of being read as
/// configuration.
///
/// Nothing errors. That is the whole point: this is the original silent drop,
/// and until now no test in this file wrote a `\r\n`.
#[test]
fn an_agent_file_with_windows_line_endings_means_what_it_says() {
    let base = scratch("agent-crlf");
    let root = base.join(".claude");
    write(
        &root.join("agents/explorer.md"),
        "---\r\nname: explorer\r\ndescription: Search a tree.\r\ntools: Read, Grep\r\n---\r\n\r\nI explore.\r\n",
    );
    let h = Harness::load_selecting(&root, Flavor::Claude, None).expect("load");
    let types = h.agent_types();
    assert_eq!(
        types.len(),
        1,
        "the CRLF agent was dropped: {:?}",
        h.agent_notes()
    );
    assert_eq!(types[0].description, "Search a tree.");
    assert_eq!(
        types[0].tools.as_deref(),
        Some(&["Read".to_string(), "Grep".to_string()][..]),
        "the tools list did not survive the line endings"
    );
    // The half that fails silently in the other direction: if the fence is not
    // recognised the whole file is body, so the configuration lines become
    // standing instructions.
    assert_eq!(
        types[0].instructions.trim(),
        "I explore.",
        "frontmatter leaked into the prompt: {:?}",
        types[0].instructions
    );
}

/// The same file wearing the other invisible byte, selected as the persona — so
/// the assertion is on the standing instructions the session actually runs with.
///
/// If this fails: the operator selected an agent, Emma started, and the prompt
/// it is running on contains three lines of YAML the model will read as orders.
#[test]
fn a_selected_agent_with_a_byte_order_mark_contributes_a_prompt_and_not_its_frontmatter() {
    let base = scratch("agent-bom");
    let root = base.join(".claude");
    write(
        &root.join("agents/explorer.md"),
        "\u{feff}---\nname: explorer\ndescription: Search a tree.\ntools: Read\n---\n\nI explore.\n",
    );
    let h = Harness::load_selecting(&root, Flavor::Claude, Some("explorer".into())).expect("load");
    assert_eq!(h.persona.as_deref(), Some("explorer"));
    assert_eq!(
        h.instructions.trim(),
        "I explore.",
        "the frontmatter is in the prompt: {:?}",
        h.instructions
    );
    assert_eq!(
        h.tools(),
        Some(&["Read".to_string()][..]),
        "the allowlist behind the mark did not apply"
    );
}

// endregion: agents/

// region: commands/ — the report nothing was reading
// ---------------------------------------------------------------------------
// commands/
//
// The loader grew a count of the files it passes over, and the count reaches
// the caller. Nothing asserted it: the only test on nesting checked
// `command_names() == ["top"]`, which stays true whether the buried files are
// counted, reported, or ignored entirely. The loader's own doc says so.
// ---------------------------------------------------------------------------

/// If this fails: an operator with a `commands/` tree organised into folders
/// sees none of those commands and is told nothing — the same quiet shortfall as
/// a dropped skill, in the surface a person types at directly.
#[test]
fn commands_in_subdirectories_are_counted_and_the_count_reaches_the_caller() {
    let base = scratch("cmd-nested-note");
    let root = base.join(".claude");
    write(&root.join("commands/top.md"), "at the top level");
    write(&root.join("commands/group/buried.md"), "in a subdirectory");
    write(
        &root.join("commands/group/also.md"),
        "in a subdirectory too",
    );
    // Not a command, and must not inflate the count.
    write(&root.join("commands/group/notes.txt"), "not a command");
    let h = Harness::load(&root).expect("load");
    assert_eq!(h.command_names(), vec!["top"]);
    let notes = h.command_notes().join("\n");
    assert!(
        notes.contains("2 command file(s) below"),
        "the buried commands were dropped without a count: {notes}"
    );
    assert!(
        notes.contains("at the top level becomes a `/name`"),
        "the note has to say why they were passed over: {notes}"
    );
}

/// If this fails: a command file that is not UTF-8 stops the session, and every
/// other command goes with it — for a file whose worst honest outcome is one
/// missing `/name`.
#[test]
fn a_command_that_is_not_utf8_costs_that_command_and_not_the_session() {
    let base = scratch("cmd-latin1");
    let root = base.join(".claude");
    write(&root.join("commands/ok.md"), "fine");
    write_bytes(&root.join("commands/bad.md"), b"caf\xe9 body");
    let h = Harness::load(&root).expect("one unreadable command must not cost the harness");
    assert_eq!(h.command_names(), vec!["ok"]);
    let notes = h.command_notes().join("\n");
    assert!(
        notes.contains("`/bad` does not exist"),
        "the command vanished without a word: {notes}"
    );
}

/// The control. If this fails, every ordinary `commands/` directory reports a
/// problem it does not have.
#[test]
fn an_ordinary_commands_directory_reports_nothing_at_all() {
    let base = scratch("cmd-quiet");
    let root = base.join(".claude");
    write(&root.join("commands/top.md"), "at the top level");
    write(&root.join("commands/other.md"), "also at the top level");
    let h = Harness::load(&root).expect("load");
    assert_eq!(h.command_names(), vec!["other", "top"]);
    assert!(
        h.command_notes().is_empty(),
        "a directory with nothing wrong with it produced a report: {:?}",
        h.command_notes()
    );
}

/// If this fails: the `description:` and `argument-hint:` lines of every command
/// written on Windows are pasted into the model's context as if the user had
/// typed them at the prompt. 59 of the 116 real commands on the owner's machine
/// carry such a block, and a Windows editor writes every one of them with CRLF.
#[test]
fn a_windows_command_file_does_not_send_its_frontmatter_to_the_model() {
    let base = scratch("cmd-crlf");
    let root = base.join(".claude");
    write(
        &root.join("commands/review.md"),
        "---\r\ndescription: Review a diff.\r\nargument-hint: <path>\r\n---\r\n\r\nRead the diff.\r\n",
    );
    let expanded = Harness::load(&root)
        .expect("load")
        .expand_command("/review")
        .expect("the command has to exist");
    assert_eq!(
        expanded.text, "Read the diff.",
        "the frontmatter reached the model as if it had been typed"
    );
}

// endregion: commands/

// region: hooks — the surface the mark reaches through settings.json
// ---------------------------------------------------------------------------
// hooks
//
// Hooks have no file of their own: they are a block inside the spine, so every
// way that file can fail to be read is a way a policy the operator believes in
// is not there. That is the one failure this crate calls worse than an outage.
// ---------------------------------------------------------------------------

/// If this fails: a `settings.json` with a byte-order mark refuses the boot, so
/// the operator's next move is to delete the block or the file — and a
/// `PreToolUse` guard they wrote is then genuinely gone rather than merely
/// unread.
///
/// Asserted through `snapshot`, which is what `emma config check` prints, rather
/// than through the private hook vector: the question is whether an operator can
/// see the hook is loaded.
#[test]
fn a_hook_block_behind_a_byte_order_mark_still_resolves() {
    let base = scratch("bom-hooks");
    let root = base.join(".claude");
    std::fs::create_dir_all(root.join("hooks")).expect("mkdir");
    let file = if cfg!(windows) {
        "guard.cmd"
    } else {
        "guard.sh"
    };
    let hook = root.join("hooks").join(file);
    std::fs::write(&hook, "@echo off\r\n").expect("write hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Windows gets executability from `.cmd`; unix requires a mode bit. A
        // missing bit would stop resolution before the BOM guarantee is tried.
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("make hook executable");
    }
    write(
        &root.join("settings.json"),
        &format!(
            "\u{feff}{}",
            format_args!(
                "{{\"hooks\":{{\"PreToolUse\":[{{\"matcher\":\"Bash\",\
                 \"hooks\":[{{\"type\":\"command\",\
                 \"command\":\"$CLAUDE_PROJECT_DIR/hooks/{file}\"}}]}}]}}}}"
            )
        ),
    );
    let h = Harness::load(&root).expect("a BOM must not cost the operator their guard");
    let snapshot = h.snapshot();
    let hooks = snapshot
        .get("hooks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        hooks.len(),
        1,
        "the hook behind the byte-order mark did not resolve: {snapshot:#}"
    );
    assert_eq!(
        hooks[0].get("event").and_then(|v| v.as_str()),
        Some("PreToolUse"),
        "the hook resolved onto some other event: {snapshot:#}"
    );
    // *Not asserted here:* that the matcher survived. `snapshot` does not carry
    // it, and whether a matcher is honoured is `matchers_are_anchored`'s
    // question in `harness_hooks.rs`, against a hook that actually fires. What
    // this test owns is the file being read at all.
}

// endregion: hooks
