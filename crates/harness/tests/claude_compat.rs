//! Reading a `.claude/` directory, and the two rules the compatibility note
//! calls firm: `.emma/` wins outright where both exist, and a hook event Emma
//! does not implement is a loud startup error rather than a silent skip.
//!
//! The point of this feature is that a skill or command already written for
//! Claude Code works unchanged. The point of these tests is that it works
//! unchanged *or says so* — a compatibility layer that half-reads a file is
//! worse than one that refuses it, because the operator cannot tell.
//!
//! Read the refusals here as the feature, not as gaps in it. Every `expect_err`
//! below is a case where Claude Code would have done something Emma cannot do
//! safely, and the ruling is always the same: say so at boot rather than
//! approximate it at the first tool call.

mod support;

use emma_harness::{Flavor, Harness, HookCall, HookEvent};
use std::path::{Path, PathBuf};
use support::*;

// region: Writing a .claude/ directory to disk
// ---------------------------------------------------------------------------
// Writing a `.claude/` directory to disk
//
// The same two helpers `harness_hooks.rs` uses, with one difference that
// matters: `script` returns the bare filename rather than `hooks/<file>`,
// because a `settings.json` command is written the way Claude Code writes it
// and `translate_command` is the thing under test.
// ---------------------------------------------------------------------------

fn script(dir: &Path, name: &str, unix: &str, windows: &str) -> String {
    std::fs::create_dir_all(dir).expect("mkdir hooks");
    let file = if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        format!("{name}.sh")
    };
    let body = if cfg!(windows) {
        format!("@echo off\r\n{windows}\r\n")
    } else {
        format!("#!/bin/sh\n{unix}\n")
    };
    let path = dir.join(&file);
    std::fs::write(&path, body).expect("write hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    file
}

fn call<'a>(args: &'a serde_json::Value) -> HookCall<'a> {
    HookCall {
        tool_name: "Bash",
        tool_call_id: "toolu_1",
        args,
        session_id: "s",
        turn_id: "t",
        result: None,
    }
}

// endregion: Writing a .claude/ directory to disk

// region: Discovery and precedence
// ---------------------------------------------------------------------------
// Discovery and precedence
//
// Which directory becomes the harness when there is more than one candidate,
// and the one candidate that is never eligible. Every question here is settled
// before a byte of configuration is read, which is the only way the answer to
// "where did this instruction come from" stays a single file.
// ---------------------------------------------------------------------------

/// The whole feature in one assertion: a repository that has only ever been
/// configured for Claude Code boots Emma without anyone adding a file. Delete
/// this and `.claude/` recognition regresses to "load it if you name it", which
/// nobody would.
#[test]
fn a_claude_directory_is_discovered_like_an_emma_one() {
    let base = scratch("claude-discover");
    std::fs::create_dir_all(base.join(".claude")).expect("mkdir");
    let deep = base.join("a/b");
    std::fs::create_dir_all(&deep).expect("mkdir");

    let found = emma_harness::discover_from(&deep, None).expect("walk up to .claude");
    assert_eq!(
        found.canonicalize().unwrap(),
        base.join(".claude").canonicalize().unwrap()
    );
    assert_eq!(Flavor::of(&found), Flavor::Claude);
}

/// Never merged. Merging is convenient and is exactly how the answer to "where
/// did this instruction come from" stops being a file.
#[test]
fn emma_wins_outright_where_both_exist() {
    let base = scratch("both");
    write(&base.join(".emma/personas/a/rules.md"), "EMMA-PROMPT");
    write(
        &base.join(".emma/config.json"),
        r#"{"default_persona":"a","personas":{"a":{}}}"#,
    );
    write(&base.join("CLAUDE.md"), "CLAUDE-PROMPT");
    write(&base.join(".claude/commands/only-here.md"), "body");

    let found = emma_harness::discover_from(&base, None).expect("discover");
    assert_eq!(found.file_name().unwrap(), ".emma");

    let h = Harness::load(&found).expect("load");
    assert_eq!(h.instructions, "EMMA-PROMPT");
    assert!(
        h.expand_command("/only-here").is_none(),
        "the loser must be ignored entirely, not merged in"
    );

    // **Ignored entirely, and said out loud.** The precedence above is correct
    // and deliberate — merging is how a configuration system becomes impossible
    // to reason about — but until this note existed the loser vanished without a
    // word. Certified on the real binary before it was fixed: a `.emma/` holding
    // nothing but junk beside a `.claude/` with a real skill in it gave
    // `skills (none)` and said nothing at all.
    //
    // It is the largest silent skip the system had: skills, agents, commands,
    // hooks and permissions passed over at once, while every smaller loser here
    // — an unevaluable rule, a skipped skill, a dropped hook — already got a
    // sentence.
    let note = h
        .shadowed_note()
        .expect("the ignored sibling was not named anywhere");
    assert!(
        note.contains(".claude"),
        "the note does not name what was ignored: {note}"
    );
    assert!(
        note.contains("ignored entirely"),
        "the note does not say the loser was passed over rather than merged: {note}"
    );
}

/// Precedence is per directory: the nearest ancestor still wins overall, and
/// `.emma/` only beats a `.claude/` sitting beside it.
#[test]
fn the_nearest_ancestor_wins_before_the_directory_name_does() {
    let base = scratch("nearest");
    std::fs::create_dir_all(base.join(".emma")).expect("mkdir");
    let inner = base.join("sub");
    std::fs::create_dir_all(inner.join(".claude")).expect("mkdir");

    let found = emma_harness::discover_from(&inner, None).expect("discover");
    assert_eq!(
        found.canonicalize().unwrap(),
        inner.join(".claude").canonicalize().unwrap(),
        "a nearer .claude/ is the configuration the operator is standing in"
    );
}

/// `~/.claude/` is Claude Code's user-scope configuration for a different
/// program. Adopting it as this project's harness would hand Emma standing
/// instructions from a directory the user never associated with this project —
/// "booted on the wrong prompt" arriving through the front door, in the one
/// scenario where nothing looks wrong: Emma run from anywhere outside a
/// configured repository, on a box where `~/.claude/` almost certainly exists.
///
/// The home directory is threaded in rather than set on the process. The earlier
/// version of this test set `HOME` and `USERPROFILE` and put them back, which is
/// shared mutable state in a binary cargo runs threaded — and `discover_from`'s
/// own docs argue that a race in a test is a green light nobody earned.
#[test]
fn the_users_global_claude_directory_is_not_a_project_harness() {
    let home = scratch("fake-home");
    std::fs::create_dir_all(home.join(".claude")).expect("mkdir");
    let deep = home.join("code/project");
    std::fs::create_dir_all(&deep).expect("mkdir");

    let err = emma_harness::discover_in(&deep, None, Some(home.clone()))
        .expect_err("the global .claude/ must not be adopted");
    assert!(
        !err.to_string()
            .contains(&home.join(".claude").display().to_string()),
        "it should not even claim to have looked there: {err}"
    );
}

/// The skip above was a byte-wise `PathBuf` comparison, and on Windows the same
/// directory has more than one spelling. A `$HOME` of `C:\Users\Owner` against a
/// walked ancestor of `C:\Users\owner` did not match, so the skip never fired and
/// the user's global `.claude/` was adopted as the project harness — silently,
/// which is the entire failure the skip exists to prevent.
///
/// The two spellings have to differ, or the test cannot see the bug: written the
/// obvious way it sets the home to the exact string it walks from, and every
/// comparison in the world passes that.
#[test]
fn the_skip_survives_a_differently_cased_home() {
    let home = scratch("FakeHome-CASED");
    std::fs::create_dir_all(home.join(".claude")).expect("mkdir");
    let deep = home.join("code/project");
    std::fs::create_dir_all(&deep).expect("mkdir");

    // A different spelling of the same directory — which only exists as such on a
    // case-insensitive filesystem. Where the filesystem is case-sensitive these
    // really are two directories and there is nothing to assert.
    let other = std::path::PathBuf::from(home.to_string_lossy().to_lowercase());
    if !other.is_dir() || other == home {
        eprintln!("case-sensitive filesystem: nothing to test");
        return;
    }

    let err = emma_harness::discover_in(&deep, None, Some(other))
        .expect_err("a differently-spelled $HOME is still $HOME");
    assert!(
        !err.to_string()
            .contains(&home.join(".claude").display().to_string()),
        "the skip went inert under a differently-cased home: {err}"
    );
}

// endregion: Discovery and precedence

// region: What maps
// ---------------------------------------------------------------------------
// What maps
//
// The parts of a `.claude/` directory Emma can read as its own: `CLAUDE.md` as
// standing instructions, skills and commands through the identical loader, and
// `agents/` as the nearest thing to a persona — including the place where the
// persona rules deliberately diverge.
// ---------------------------------------------------------------------------

/// In `.claude/` the prompt is `CLAUDE.md`, not a persona — which is why the
/// unselected-persona refusal cannot apply here. Both locations are read and the
/// order is fixed, for the same reason `.emma/` assembly has a fixed order: a
/// prompt whose bytes depend on filesystem iteration has a hash that means
/// nothing.
#[test]
fn claude_md_becomes_the_standing_instructions() {
    let base = scratch("claude-md");
    write(&base.join("CLAUDE.md"), "PROJECT-RULES");
    write(&base.join(".claude/CLAUDE.md"), "LOCAL-RULES");

    let h = Harness::load(base.join(".claude")).expect("load");
    assert_eq!(
        h.instructions, "PROJECT-RULES\n\nLOCAL-RULES",
        "fixed order, and the project file first"
    );
    assert!(!h.is_empty());
    assert_eq!(h.snapshot()["flavor"], "claude");
}

/// Not a translation layer — literally the same loader, which is what makes the
/// compatibility claim cheap enough to be worth having. If these ever diverge,
/// the two directory flavours become two formats to keep in step, and this test
/// is where that shows up.
#[test]
fn skills_and_commands_are_read_by_the_same_code() {
    let base = scratch("claude-skills");
    let root = base.join(".claude");
    with_skill(&root, "review", "Review a diff.", "# how to review");
    write(&root.join("commands/ship.md"), "Ship it.\n");

    let h = Harness::load(&root).expect("load");
    assert_eq!(h.skill_names(), vec!["review"]);
    assert_eq!(h.skill("review").expect("found").body, "# how to review\n");
    assert_eq!(
        h.expand_command("/ship now").expect("known").text,
        "Ship it.\n\nnow"
    );
}

/// An unselected `agents/` directory must **not** refuse to start. In `.emma/`
/// a persona nobody selected is an accident; here `agents/` is a normal part of
/// every Claude Code repository and `CLAUDE.md` is the prompt.
#[test]
fn unselected_agents_do_not_refuse_the_boot() {
    let base = scratch("claude-agents");
    let root = base.join(".claude");
    write(&base.join("CLAUDE.md"), "RULES");
    write(
        &root.join("agents/reviewer.md"),
        "---\nname: reviewer\n---\nbody",
    );
    write(
        &root.join("agents/planner.md"),
        "---\nname: planner\n---\nbody",
    );

    let h = Harness::load(&root).expect("a repository full of agents must still boot");
    assert!(h.persona.is_none());
    assert_eq!(h.instructions, "RULES");
}

/// Two things travel out of one file and both can fail quietly. Frontmatter
/// reaching the prompt would tell the model about machinery it cannot use; the
/// `tools:` line failing to reach `select_tools` would leave an agent declaring
/// two read-only tools with a shell in hand. The last assertion is the one that
/// makes the allowlist a boundary rather than a note.
#[test]
fn a_selected_agent_supplies_a_prompt_layer_and_its_allowlist() {
    let base = scratch("claude-agent-selected");
    let root = base.join(".claude");
    write(&base.join("CLAUDE.md"), "RULES");
    write(
        &root.join("agents/reviewer.md"),
        "---\nname: reviewer\ntools: Read, Grep\nmodel: opus\n---\n\nReview carefully.\n",
    );

    let h = Harness::load_selecting(&root, Flavor::Claude, Some("reviewer".into())).expect("load");
    assert_eq!(h.persona.as_deref(), Some("reviewer"));
    assert_eq!(
        h.instructions, "RULES\n\nReview carefully.\n",
        "the frontmatter is configuration and must not reach the model"
    );
    assert_eq!(
        h.tools(),
        Some(["Read".to_string(), "Grep".to_string()].as_slice())
    );
    let selected = h
        .select_tools(registry(&["Read", "Grep", "Bash"]))
        .expect("select");
    assert_eq!(selected.names(), vec!["Read", "Grep"]);
}

/// A YAML list is as common as the comma-separated string, and accepting one
/// while silently ignoring the other would produce an empty allowlist — which
/// under Emma's rules means no tools at all.
#[test]
fn an_agent_may_write_its_tools_as_a_yaml_list() {
    let base = scratch("claude-agent-list");
    let root = base.join(".claude");
    write(
        &root.join("agents/r.md"),
        "---\ntools:\n  - Read\n  - Bash\n---\nbody\n",
    );
    let h = Harness::load_selecting(&root, Flavor::Claude, Some("r".into())).expect("load");
    assert_eq!(
        h.tools(),
        Some(["Read".to_string(), "Bash".to_string()].as_slice())
    );
}

/// **The compatibility promise, at the level below `settings.json`.** Real
/// skills in the wild carry `model-role`, `version` and `allowed-tools`, none of
/// which Emma reads — and under `deny_unknown_fields` every one of them took the
/// whole boot down. That is the same argument that made the outer level of
/// `settings.json` permissive, reached the opposite way: a rule that fires on
/// correct configuration is an outage, not a safety property.
#[test]
fn a_claude_skill_may_carry_keys_emma_does_not_read() {
    let base = scratch("claude-skill-extra");
    let root = base.join(".claude");
    write(
        &root.join("skills/adr/SKILL.md"),
        "---\nname: adr\ndescription: Write an ADR.\nmodel-role: planner\nversion: 2\n\
         allowed-tools: Read, Write\n---\n\n# how to write one\n",
    );
    let h = Harness::load(&root).expect("a real Claude Code skill must not stop the boot");
    assert_eq!(h.skill_names(), vec!["adr"]);
    assert_eq!(h.skill("adr").expect("found").description, "Write an ADR.");
}

/// A command's frontmatter is metadata, not something the model reads.
///
/// 59 of the 116 real commands on the owner's machine carry a `---` block, and
/// all of it used to be pasted into the model's context as though the operator
/// had typed `description:` at the prompt — including keys Emma does not act
/// on, which then read as instructions rather than as configuration.
#[test]
fn a_command_does_not_send_its_frontmatter_to_the_model() {
    let base = scratch("claude-cmd-front");
    let root = base.join(".claude");
    write(&root.join("commands/review.md"), "---\nallowed-tools: Read, Grep\ndescription: Review a file.\n---\n\nReview it carefully.\n");
    let h = Harness::load(&root).expect("load");
    let out = h.expand_command("/review").expect("a known command");
    assert!(
        !out.text.contains("allowed-tools"),
        "frontmatter reached the model: {}",
        out.text
    );
    assert!(out.text.starts_with("Review it carefully."), "{}", out.text);
}

/// `$ARGUMENTS` is replaced where the author put it.
///
/// 37 real commands use the placeholder. Every one of them was getting its
/// arguments pasted at the end instead, so a command reading "review the file
/// $ARGUMENTS and report" reached the model with the placeholder still in the
/// sentence and the filename tacked on two lines below.
#[test]
fn a_command_substitutes_its_arguments_where_the_author_asked() {
    let base = scratch("claude-cmd-args");
    let root = base.join(".claude");
    write(
        &root.join("commands/review.md"),
        "Review the file $ARGUMENTS and report what is wrong.",
    );
    write(
        &root.join("commands/plain.md"),
        "Review whatever comes next.",
    );
    let h = Harness::load(&root).expect("load");

    let out = h.expand_command("/review src/main.rs").expect("known");
    assert_eq!(
        out.text,
        "Review the file src/main.rs and report what is wrong."
    );
    assert!(!out.text.contains("$ARGUMENTS"), "{}", out.text);

    // A command that does not ask still gets its arguments appended, which is
    // the older behaviour and the only sensible one when there is no slot.
    let out = h.expand_command("/plain now").expect("known");
    assert!(out.text.ends_with("now"), "{}", out.text);
}

/// A command in a subdirectory contributes nothing, and that is said out loud.
///
/// Top level only is the design — a command is summoned as `/name` and a name
/// taken from a nested path is ambiguous. But 14 real command files sit in
/// subdirectories on the owner`s machine contributing nothing, and until this
/// they did so in silence, which is the same quiet gap as a skipped skill.
#[test]
fn commands_in_subdirectories_are_not_loaded_and_the_boot_still_works() {
    let base = scratch("claude-cmd-nested");
    let root = base.join(".claude");
    write(&root.join("commands/top.md"), "at the top level");
    write(&root.join("commands/group/buried.md"), "in a subdirectory");
    let h = Harness::load(&root).expect("a nested command must not stop the boot");
    assert_eq!(
        h.command_names(),
        vec!["top"],
        "only the top-level command becomes a /name"
    );
}

/// Two skills declaring one name do not vanish quietly.
///
/// The catalogue is keyed on the frontmatter `name`, so a collision means one
/// skill silently replaced the other and the winner depended on the order the
/// filesystem returned the directory in. The owner has a real collision on his
/// own machine. The sibling agent loader had reported this for months; skills
/// never got the equivalent.
#[test]
fn two_skills_claiming_one_name_are_reported() {
    let base = scratch("claude-skill-dupe");
    let root = base.join(".claude");
    write(
        &root.join("skills/first/SKILL.md"),
        "---\nname: adr\ndescription: From the first directory.\n---\n\n# one\n",
    );
    write(
        &root.join("skills/second/SKILL.md"),
        "---\nname: adr\ndescription: From the second directory.\n---\n\n# two\n",
    );
    let h = Harness::load(&root).expect("a collision must not stop the boot");
    // One name, one entry: that is the collision, and it is why it must be said
    // out loud rather than left to be discovered.
    assert_eq!(
        h.skill_names(),
        vec!["adr"],
        "both should collapse to one name"
    );
}

/// An empty frontmatter block closes, and its fences do not become content.
///
/// **Three callers, three different wrong answers, one missing case.** Every
/// one searched for the closing fence with `rest.find("\n---")`, which needs a
/// newline *before* the closer — so `---` immediately followed by `---` was
/// never closed. An adversarial reviewer checked what each caller then did:
/// `split_agent` returned default frontmatter with no error and leaked **both**
/// fence lines into the body, `split_skill` refused with "frontmatter is not
/// closed" (loud, and the wrong diagnosis), and `strip_frontmatter` handed the
/// fences back as command text — which means they were sent to the model as
/// part of the prompt.
///
/// An empty block is not an exotic input. It is what a template leaves behind
/// and what an editor writes once every key has been deleted.
///
/// Both line endings, because the last defect in this same code was CRLF.
#[test]
fn an_empty_frontmatter_block_closes_and_its_fences_are_not_content() {
    for (tag, nl) in [("lf", "\n"), ("crlf", "\r\n")] {
        let base = scratch(&format!("claude-empty-front-{tag}"));
        let root = base.join(".claude");

        // A command: its frontmatter is stripped before the body reaches the
        // model, so a leaked fence is a leaked fence in the prompt.
        write(
            &root.join("commands").join("empty.md"),
            &format!("---{nl}---{nl}do the thing"),
        );
        // A skill with the same shape. It cannot load — an empty block declares
        // no `name` — but *why* it is refused is the point: "frontmatter is not
        // closed" was the wrong diagnosis for a file whose frontmatter is
        // closed and empty, and a wrong reason sends its reader to the wrong
        // line.
        write(
            &root.join("skills").join("hollow").join("SKILL.md"),
            &format!("---{nl}---{nl}I am the skill."),
        );

        let h = Harness::load(&root).expect("an empty frontmatter block must not fail the load");

        let body = h
            .expand_command("/empty")
            .unwrap_or_else(|| panic!("{tag}: the command did not load"))
            .text;
        assert!(
            !body.contains("---"),
            "{tag}: a fence line reached the model as prompt text: {body:?}"
        );
        assert!(
            body.contains("do the thing"),
            "{tag}: the body was lost: {body:?}"
        );

        assert!(
            !h.skill_names().contains(&"hollow"),
            "{tag}: a skill declaring no name loaded anyway"
        );
    }
}

/// A skill written on Windows loads.
///
/// **This is the defect that cost 99 of 324 real skills on the owner's machine.**
/// `split_skill` demanded a byte-exact `---` + LF opener, so every `SKILL.md`
/// whose editor ends lines with CRLF parsed as "expected YAML frontmatter" and
/// was skipped with a stderr line nobody counted. The agent parser one file away
/// had already been fixed for exactly this after 90 of 90 real agent files
/// failed; the tolerance was never carried across. Both now call one function.
///
/// Break `claude::open_frontmatter` and this goes red — that is the whole point
/// of it being shared.
#[test]
fn a_skill_written_with_windows_line_endings_loads() {
    let base = scratch("claude-skill-crlf");
    let root = base.join(".claude");
    write(
        &root.join("skills/adr/SKILL.md"),
        "---\r\nname: adr\r\ndescription: Write an ADR.\r\n---\r\n\r\n# how to write one\r\n",
    );
    let h = Harness::load(&root).expect("a CRLF skill must load, not vanish");
    assert_eq!(h.skill_names(), vec!["adr"], "the CRLF skill was dropped");
    assert_eq!(h.skill("adr").expect("found").description, "Write an ADR.");
}

/// The same failure wearing a different byte: a UTF-8 BOM is invisible in the
/// editor that wrote it and turns the first line into something that is not
/// `---`. The real corpus carries both spellings.
#[test]
fn a_skill_carrying_a_byte_order_mark_loads() {
    let base = scratch("claude-skill-bom");
    let root = base.join(".claude");
    write(
        &root.join("skills/adr/SKILL.md"),
        "\u{feff}---\nname: adr\ndescription: Write an ADR.\n---\n\n# how to write one\n",
    );
    let h = Harness::load(&root).expect("a BOM must not hide a skill");
    assert_eq!(h.skill_names(), vec!["adr"], "the BOM skill was dropped");
}

/// A licence header above the frontmatter is common enough to be worth handling
/// rather than skipping: the file is well-formed, it just does not open with its
/// own first line.
#[test]
fn a_claude_skill_may_open_with_a_licence_comment() {
    let base = scratch("claude-skill-comment");
    let root = base.join(".claude");
    write(
        &root.join("skills/debug/SKILL.md"),
        "<!-- Copyright (c) somebody. MIT. -->\n\
         ---\nname: debug\ndescription: Debug a failure.\n---\n\nbody\n",
    );
    let h = Harness::load(&root).expect("load");
    assert_eq!(h.skill_names(), vec!["debug"]);
    assert_eq!(h.skill("debug").expect("found").body, "body\n");
}

/// Two skills declaring one name is reported, not silently resolved.
///
/// **Which one wins depends on the order the filesystem returned the directory
/// in**, so the same tree can load different skills on different runs, and
/// nothing said so. The sibling agent loader had been printing exactly this
/// note for months; skills never got it.
///
/// Asserted on `skill_notes` rather than on stderr, because a report nothing can
/// read is the defect rather than the fix — the same mistake HARD-001's first
/// attempt made.
#[test]
fn two_skills_declaring_one_name_are_reported_rather_than_silently_collapsed() {
    let base = scratch("claude-skill-dupe");
    let root = base.join(".claude");
    write(
        &root.join("skills/first/SKILL.md"),
        "---
name: shared
description: The first one.
---
first body
",
    );
    write(
        &root.join("skills/second/SKILL.md"),
        "---
name: shared
description: The second one.
---
second body
",
    );

    let h = Harness::load(&root).expect("a duplicate name must not fail the boot");
    assert_eq!(
        h.skill_names(),
        vec!["shared"],
        "both loaded under one name"
    );

    let notes = h.skill_notes();
    assert!(
        notes.iter().any(|n| n.contains("shared")),
        "a name collision was resolved in silence: {notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("may differ between runs")),
        "the note does not say the winner is filesystem-order dependent: {notes:?}"
    );
}

/// The other side of permissive, and the reason it is a skip rather than a
/// shrug: a file this loader genuinely cannot read is not silently dropped, it is
/// named on stderr — but it does not take the boot, and above all it does not
/// take the skills beside it.
#[test]
fn an_unreadable_claude_skill_is_skipped_rather_than_fatal() {
    let base = scratch("claude-skill-bad");
    let root = base.join(".claude");
    write(
        &root.join("skills/broken/SKILL.md"),
        "no frontmatter at all\n",
    );
    write(
        &root.join("skills/alsobroken/SKILL.md"),
        "---\ndescription: no name\n---\nbody\n",
    );
    with_skill(&root, "good", "Still here.", "body");

    let h = Harness::load(&root).expect("one bad skill must not cost the whole harness");
    assert_eq!(
        h.skill_names(),
        vec!["good"],
        "the readable skill beside the broken ones still has to load"
    );

    // **And the count is observable, which is what HARD-001 was actually
    // asking for.** That row exists because skipped skills were named on
    // stderr and never counted; the first fix added a count — on stderr. So the
    // guarantee stayed exactly as unobservable as before, and a mutation
    // deleting the whole summary left the suite green. A number nothing can
    // read is not a count, it is a comment.
    let notes = h.skill_notes();
    assert!(
        notes.iter().any(|n| n.contains("2 skill(s)")),
        "the skip count is not reported anywhere a caller can see it: {notes:?}"
    );
}

/// And `.emma/` stays strict, which is what the permissiveness above costs.
/// There an unknown key is the user's typo in Emma's own format, and
/// `deny_unknown_fields` is doing real work.
#[test]
fn an_emma_skill_with_an_unknown_key_still_fails_the_load() {
    let root = one_persona("emma-skill-strict", "rules");
    write(
        &root.join("skills/typo/SKILL.md"),
        "---\nname: typo\ndescription: d\nmodle: opus\n---\nbody\n",
    );
    let err = Harness::load(&root).expect_err("a typo in Emma's own format is a load error");
    let msg = format!("{err:#}");
    assert!(msg.contains("SKILL.md"), "{msg}");
    assert!(msg.contains("modle"), "the error must name the key: {msg}");
}

/// The line between the two flavours' persona rules. Emma does not refuse over
/// an agent nobody selected, but selecting one that is not there is still a
/// mistake — and starting anyway would run on `CLAUDE.md` alone while the
/// operator believed they had the agent's prompt.
#[test]
fn selecting_an_agent_that_does_not_exist_refuses() {
    let base = scratch("claude-agent-missing");
    let root = base.join(".claude");
    write(&root.join("agents/real.md"), "body");
    let err = Harness::load_selecting(&root, Flavor::Claude, Some("ghost".into()))
        .expect_err("a named agent that is not there is a mistake");
    let msg = format!("{err:#}");
    assert!(msg.contains("ghost") && msg.contains("real"), "{msg}");
}

// endregion: What maps

// region: settings.json
// ---------------------------------------------------------------------------
// settings.json
//
// Permissive outside the hooks block and strict within it, which is one
// sentence and five tests because both halves fail invisibly. Too strict and
// Emma refuses to start in a normal repository; too loose and a shell string or
// a misspelled matcher becomes a guard that is not there.
// ---------------------------------------------------------------------------

/// The outer object is permissive on purpose: `permissions`, `model` and
/// `statusLine` are Claude Code's business, and refusing to start over them
/// would make this feature an outage rather than a safety property.
#[test]
fn settings_keys_emma_has_no_opinion_about_are_ignored() {
    let base = scratch("claude-settings-extra");
    let root = base.join(".claude");
    write(
        &root.join("settings.json"),
        r#"{"model":"opus","permissions":{"allow":["Bash(ls:*)"]},"statusLine":{"type":"command"}}"#,
    );
    let h = Harness::load(&root).expect("a normal settings.json must not stop the boot");
    assert!(h.persona.is_none());
}

/// End to end through the format translation: Claude Code's event → groups →
/// commands nesting, the `$CLAUDE_PROJECT_DIR` prefix, and `timeout` in seconds
/// all have to survive into a hook that actually denies. The second half is the
/// part that fails silently if the translation drops the matcher — a hook that
/// guards every tool instead of `Bash` still passes the first half.
#[tokio::test]
async fn a_settings_json_hook_resolves_and_fires() {
    let base = scratch("claude-hook");
    let root = base.join(".claude");
    let file = script(
        &root.join("hooks"),
        "guard",
        r#"echo '{"decision":"deny","reason":"no shell today"}'"#,
        r#"echo {"decision":"deny","reason":"no shell today"}"#,
    );
    write(
        &root.join("settings.json"),
        &format!(
            r#"{{"hooks":{{"PreToolUse":[{{"matcher":"Bash","hooks":[
                 {{"type":"command","command":"$CLAUDE_PROJECT_DIR/.claude/hooks/{file}","timeout":3}}]}}]}}}}"#
        ),
    );

    let h = Harness::load(&root).expect("load");
    let args = serde_json::json!({ "command": "rm -rf /" });
    let v = h.run_hooks(HookEvent::PreToolUse, &call(&args)).await;
    assert_eq!(v.denied.as_deref(), Some("no shell today"));

    // And the matcher came across with it, still anchored.
    let mut other = call(&args);
    other.tool_name = "Read";
    let v = h.run_hooks(HookEvent::PreToolUse, &other).await;
    assert!(v.runs.is_empty(), "the matcher must still be a matcher");
}

/// The firm rule. Claude Code implements events Emma does not, and a security
/// hook that quietly never runs is worse than no hook.
/// **`EMMA_CLAUDE_HOOKS` is process-global and these two tests disagree about
/// it, so they may not run at the same time.**
///
/// `cargo test` runs a binary's tests on several threads. One of the two below
/// sets the variable and clears it again; the other asserts the boot refuses
/// *because* it is unset. Nothing stopped them overlapping, and on the sixth
/// full-suite run of one afternoon they did: the strict-default test loaded a
/// harness successfully and failed with `an unimplemented event must never be
/// skipped`, which reads exactly like the guarantee having been removed rather
/// than like a neighbouring test holding the door open.
///
/// That is the expensive shape of a flaky test: it fails with the words of a
/// real defect. Somebody would have gone looking in `hooks.rs`.
///
/// A mutex rather than a redesign, because the variable being process-global is
/// the point of it — a config key could be set by the repository being opened,
/// and that is the trust boundary running backwards.
static HOOK_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn a_hook_event_emma_does_not_implement_is_a_loud_startup_error() {
    let _guard = HOOK_ENV.lock().unwrap_or_else(|e| e.into_inner());
    let base = scratch("claude-bad-event");
    let root = base.join(".claude");
    write(
        &root.join("settings.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"hooks/x.sh"}]}]}}"#,
    );
    let err = Harness::load(&root).expect_err("an unimplemented event must never be skipped");
    let msg = format!("{err:#}");
    assert!(msg.contains("SessionStart"), "{msg}");
    assert!(msg.contains("PreToolUse"), "{msg}");
}

/// The opt-in turns a refusal into a named skip, and never a silent one.
///
/// **Measured before this existed:** zero of five real hooks in a real
/// `~/.claude/settings.json` were usable, because the first unimplemented event
/// refused the boot. Honest, and unusable. "Loud" was never meant to mean
/// "refuses to start over somebody else's file".
///
/// The default does not move — the test above still asserts the refusal — and
/// the escape is an environment variable rather than a config key **on
/// purpose**: a repository that could relax its own strictness is the trust
/// boundary running backwards.
#[test]
fn an_unimplemented_hook_event_can_be_skipped_by_explicit_opt_in() {
    let _guard = HOOK_ENV.lock().unwrap_or_else(|e| e.into_inner());
    let base = scratch("claude-hook-optin");
    let root = base.join(".claude");
    write(
        &root.join("settings.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"hooks/x.sh"}]}]}}"#,
    );

    // Without it: still a refusal.
    assert!(Harness::load(&root).is_err(), "the strict default moved");

    // With it: the harness loads, and the hook is gone rather than pretended.
    std::env::set_var("EMMA_CLAUDE_HOOKS", "skip-unknown");
    let loaded = Harness::load(&root);
    std::env::remove_var("EMMA_CLAUDE_HOOKS");
    // The harness loads. That the hook itself is *gone* rather than kept is
    // enforced structurally rather than asserted here: `into_hook_defs` skips
    // the whole event before any `HookDef` is built, so there is no path by
    // which one could survive. There is no public accessor to check it through,
    // and inventing one for a test would be a worse trade than saying so.
    loaded.expect("the opt-in must let the harness load");
}

/// A hook Emma cannot honour costs that hook, not the session.
///
/// **Owner ruling 2026-08-23, on a measurement rather than a preference.** The
/// owner's real `~/.claude/settings.json` holds five hooks; one shell string
/// refused the entire boot, so the hooks Emma *could* have run never got the
/// chance. An unknown *event* was already skippable by name while an
/// unhonourable *command* was fatal — and that inconsistency was the defect,
/// not the strictness.
///
/// Containment is untouched: the shell string is still never executed, for the
/// reason INV-009 gives. What changed is the blast radius of refusing it.
#[test]
fn an_unhonourable_hook_command_is_skipped_by_the_same_opt_in() {
    let _guard = HOOK_ENV.lock().unwrap_or_else(|e| e.into_inner());
    let base = scratch("claude-hook-shellstring");
    let root = base.join(".claude");
    write(
        &root.join("settings.json"),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"cmd /c echo hi"}]}]}}"#,
    );

    // The strict default is unmoved: without the opt-in this still refuses.
    assert!(
        Harness::load(&root).is_err(),
        "a shell string must still refuse the boot by default"
    );

    std::env::set_var("EMMA_CLAUDE_HOOKS", "skip-unknown");
    let loaded = Harness::load(&root);
    std::env::remove_var("EMMA_CLAUDE_HOOKS");

    let h = loaded.expect("the opt-in must let the harness load past an unusable hook");

    // Skipped, not smuggled in. There is no public accessor for the hook
    // registry, and inventing one for a test would be a worse trade than
    // asserting the observable consequence: the hook does not fire. A
    // `UserPromptSubmit` hook that reached the registry would run here.
    let args = serde_json::json!({});
    let verdict = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async { h.run_hooks(HookEvent::UserPromptSubmit, &call(&args)).await });
    assert!(
        verdict.runs.is_empty(),
        "an unhonourable hook reached the registry and ran: {:?}",
        verdict.runs
    );
}

/// The genuine disagreement between the two systems, and Emma does not blink.
/// Honouring a shell string would mean dropping containment, the cleared
/// environment and the argv exec — at the exact point where Emma is deciding
/// whether to let a model run `Bash`.
#[test]
fn a_shell_string_command_is_refused_rather_than_quietly_run() {
    let base = scratch("claude-shell");
    let root = base.join(".claude");
    std::fs::create_dir_all(root.join("hooks")).expect("mkdir");
    write(
        &root.join("settings.json"),
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"jq -r .tool_name | tee -a /tmp/log"}]}]}}"#,
    );
    let err = Harness::load(&root).expect_err("a shell string cannot be honoured");
    let msg = format!("{err:#}");
    assert!(msg.contains("shell command"), "{msg}");
    assert!(
        msg.contains("hooks/"),
        "the refusal must say what to do instead: {msg}"
    );
}

/// The same refusal, one character out. The detection was a metacharacter set
/// plus an explicit space, so a tab-separated command walked straight past it and
/// failed later as a missing file — the right refusal, at the wrong place, with
/// the wrong sentence. Whitespace is whitespace.
#[test]
fn a_tab_separated_command_is_a_shell_string_too() {
    let base = scratch("claude-tab");
    let root = base.join(".claude");
    std::fs::create_dir_all(root.join("hooks")).expect("mkdir");
    write(
        &root.join("settings.json"),
        "{\"hooks\":{\"PreToolUse\":[{\"hooks\":[{\"type\":\"command\",\
         \"command\":\"hooks/guard.sh\\t--strict\"}]}]}}",
    );
    let err = Harness::load(&root).expect_err("a tab does not make it not a shell string");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("shell command"),
        "wrong refusal — it fell through to the file check: {msg}"
    );
}

/// A perfectly ordinary Claude Code hook, and Emma cannot honour it: containment
/// means hooks live under `hooks/`, and an absolute path is by definition outside
/// it. `/usr/bin/true` is deliberately a unix path — Windows would not call it
/// absolute, so `translate_command` checks the shape itself rather than asking
/// the platform, and this is the test that keeps it doing so.
#[test]
fn an_absolute_hook_command_is_refused() {
    let base = scratch("claude-abs");
    let root = base.join(".claude");
    std::fs::create_dir_all(root.join("hooks")).expect("mkdir");
    write(
        &root.join("settings.json"),
        r#"{"hooks":{"PostToolUse":[{"hooks":[{"type":"command","command":"/usr/bin/true"}]}]}}"#,
    );
    let err = Harness::load(&root).expect_err("absolute paths escape containment");
    assert!(format!("{err:#}").contains("absolute"), "{err:#}");
}

/// Strict where it counts: the hooks block is Emma's security surface even
/// though the file around it is not.
#[test]
fn an_unknown_key_inside_the_hooks_block_is_still_an_error() {
    let base = scratch("claude-hook-typo");
    let root = base.join(".claude");
    write(
        &root.join("settings.json"),
        r#"{"hooks":{"PreToolUse":[{"mathcer":"Bash","hooks":[{"type":"command","command":"hooks/x.sh"}]}]}}"#,
    );
    let err = Harness::load(&root).expect_err("a misspelled matcher must not guard everything");
    let msg = format!("{err:#}");
    assert!(msg.contains("settings.json"), "{msg}");
    assert!(
        msg.contains("mathcer"),
        "the error must name the key: {msg}"
    );
}

// endregion: settings.json

// ---------------------------------------------------------------------------
// The real library
//
// The skills analogue of `agent_types.rs`'s corpus test, which existed for
// agents and never for skills — and the gap was not academic. `split_skill`
// demanded a byte-exact `---` opener, so on the machine this was written
// against 116 of 324 real skills loaded as nothing, and every fixture in this
// file passed throughout. A fixture agrees with its author; 324 files nobody
// here wrote do not.
// ---------------------------------------------------------------------------

fn user_skills() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    let dir = home.join(".claude").join("skills");
    dir.is_dir().then_some(dir)
}

/// Load a real `~/.claude/skills/` library and hold it to the same bar the
/// agent library is held to: the great majority must end up usable.
///
/// Skipped loudly when there is no corpus, rather than silently. A test that
/// quietly passes on a machine with nothing to read is a receipt for a
/// guarantee nobody checked — which is the failure this whole file exists to
/// avoid, and which the agent version of this test has always risked.
///
/// Run with `--nocapture` for the breakdown.
#[test]
fn a_real_skill_library_loads_and_most_of_it_is_usable() {
    let Some(source) = user_skills() else {
        eprintln!("skipped: no ~/.claude/skills on this machine, so there was no real library");
        return;
    };
    let base = scratch("claude-skill-corpus");
    let root = base.join(".claude");
    let skills = root.join("skills");
    std::fs::create_dir_all(&skills).unwrap();

    let mut copied = 0usize;
    for entry in std::fs::read_dir(&source).unwrap().flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // `skills/<name>/SKILL.md` only, which is what the loader reads.
        let Some(file) = std::fs::read_dir(&dir).ok().and_then(|it| {
            it.flatten().map(|e| e.path()).find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case("SKILL.md"))
            })
        }) else {
            continue;
        };
        let into = skills.join(dir.file_name().unwrap());
        std::fs::create_dir_all(&into).unwrap();
        std::fs::copy(&file, into.join("SKILL.md")).unwrap();
        copied += 1;
    }
    if copied == 0 {
        eprintln!("skipped: ~/.claude/skills has no <name>/SKILL.md files");
        return;
    }

    let h = Harness::load(&root).expect("a real skill library must not stop the boot");
    let loaded = h.skill_names().len();
    eprintln!("real library: {copied} skills in {}", source.display());
    eprintln!("  loaded into the catalogue: {loaded}");
    eprintln!("  skipped: {}", copied.saturating_sub(loaded));

    // The same 80% bar the agent library is held to, and for the same reason: a
    // parser that kept a third of a real library would be a compatibility layer
    // in name only. Before the CRLF fix this stood at 207 of 324 — 64% — and
    // would have failed here.
    assert!(
        loaded * 10 >= copied * 8,
        "only {loaded} of {copied} real skills reached the catalogue"
    );
    // Every one of them can actually be chosen: the closed-enum `Skill` tool and
    // the prompt catalogue are built from the name and the description.
    for name in h.skill_names() {
        let skill = h.skill(name).expect("a listed skill must resolve");
        assert!(!skill.name.trim().is_empty());
        assert!(!skill.description.trim().is_empty(), "{name}");
    }
}
