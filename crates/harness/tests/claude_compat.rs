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
use std::path::Path;
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
/// Unlike the override, `$HOME` is read from the process rather than threaded
/// in, so this test has to set it and put it back. That is shared mutable state
/// in a binary the harness runs threaded, and it is the one place in this crate
/// where a test does what `discover_from`'s own docs argue tests should not.
#[test]
fn the_users_global_claude_directory_is_not_a_project_harness() {
    let home = scratch("fake-home");
    std::fs::create_dir_all(home.join(".claude")).expect("mkdir");
    let deep = home.join("code/project");
    std::fs::create_dir_all(&deep).expect("mkdir");

    // `discover_from` consults the real home, so point the walk at a tree whose
    // top *is* the home it will be compared against.
    let previous = (std::env::var_os("HOME"), std::env::var_os("USERPROFILE"));
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    let result = emma_harness::discover_from(&deep, None);
    match previous.0 {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match previous.1 {
        Some(v) => std::env::set_var("USERPROFILE", v),
        None => std::env::remove_var("USERPROFILE"),
    }

    // The walk continues past the fake home into the real one, so assert the
    // thing that matters rather than that it found nothing at all: the user's
    // global `.claude/` was not the answer, and was not even claimed as looked at.
    let global = home.join(".claude");
    match result {
        Ok(found) => assert_ne!(found, global, "the global .claude/ must not be adopted"),
        Err(e) => assert!(
            !e.to_string().contains(&global.display().to_string()),
            "it should not even claim to have looked there: {e}"
        ),
    }
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
    write(&root.join("agents/reviewer.md"), "---\nname: reviewer\n---\nbody");
    write(&root.join("agents/planner.md"), "---\nname: planner\n---\nbody");

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

    let h = Harness::load_selecting(&root, Flavor::Claude, Some("reviewer".into()))
        .expect("load");
    assert_eq!(h.persona.as_deref(), Some("reviewer"));
    assert_eq!(
        h.instructions, "RULES\n\nReview carefully.\n",
        "the frontmatter is configuration and must not reach the model"
    );
    assert_eq!(h.tools(), Some(["Read".to_string(), "Grep".to_string()].as_slice()));
    let selected = h.select_tools(registry(&["Read", "Grep", "Bash"])).expect("select");
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
    assert_eq!(h.tools(), Some(["Read".to_string(), "Bash".to_string()].as_slice()));
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
#[test]
fn a_hook_event_emma_does_not_implement_is_a_loud_startup_error() {
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
    assert!(msg.contains("mathcer"), "the error must name the key: {msg}");
}

// endregion: settings.json
