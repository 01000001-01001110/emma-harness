//! Discovery, the boot states, and the byte-exactness of assembly.
//!
//! The load path is the only place where a mistake is silent: an agent that
//! booted on the wrong prompt does not crash, it acts fluently and attributes
//! the action to an `instructions_hash` nobody reviewed — and Emma acts by
//! writing files and running commands. Every test here is a case where the
//! honest answer is "do not start".

mod support;

use emma_harness::{Flavor, Harness};
use support::*;

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

#[test]
fn discovery_walks_up_like_git_finds_dot_git() {
    let base = scratch("discover");
    let root = base.join(".emma");
    std::fs::create_dir_all(root.join("personas")).expect("mkdir");
    let deep = base.join("a/b/c");
    std::fs::create_dir_all(&deep).expect("mkdir");

    let found = emma_harness::discover_from(&deep, None).expect("walk up to .emma");
    assert_eq!(
        found.canonicalize().unwrap(),
        root.canonicalize().unwrap(),
        "this has to work from any directory in the tree"
    );
}

#[test]
fn the_env_override_wins_over_the_walk() {
    let base = scratch("override");
    let near = base.join(".emma");
    let far = scratch("override-target").join("elsewhere");
    std::fs::create_dir_all(&near).expect("mkdir");
    std::fs::create_dir_all(&far).expect("mkdir");

    let found = emma_harness::discover_from(&base, Some(far.clone())).expect("override honoured");
    assert_eq!(found, far);
}

/// An override pointing nowhere must not quietly fall back to the walk — that
/// would boot the agent on configuration nobody asked for.
#[test]
fn an_override_pointing_at_nothing_refuses_rather_than_falling_back() {
    let base = scratch("override-bad");
    std::fs::create_dir_all(base.join(".emma")).expect("mkdir");
    let err = emma_harness::discover_from(&base, Some(base.join("nope")))
        .expect_err("a broken override must not silently search");
    assert!(err.to_string().contains("EMMA_ROOT"), "{err}");
}

// ---------------------------------------------------------------------------
// The boot states
// ---------------------------------------------------------------------------

#[test]
fn an_absent_harness_refuses_to_start_and_names_what_it_searched() {
    let deep = scratch("absent").join("x/y");
    std::fs::create_dir_all(&deep).expect("mkdir");
    let err = emma_harness::discover_from(&deep, None).expect_err("absent must refuse");
    let msg = err.to_string();
    assert!(msg.contains(".emma"), "{msg}");
    assert!(
        msg.contains(&deep.join(".emma").display().to_string()),
        "the refusal must name the paths searched, so an operator can see where \
         to put it. Got: {msg}"
    );
    // The whole ruling in one assertion: no compiled-in fallback prompt.
    assert!(!msg.contains("using built-in"), "{msg}");
}

/// The empty-harness boot, and the reason it matters: it proves the harness is
/// separable from the engine. No persona content, no prompt, no special case
/// anywhere in the loop.
#[test]
fn an_empty_harness_boots_with_no_instructions() {
    let root = scratch("empty").join(".emma");
    std::fs::create_dir_all(&root).expect("mkdir");

    let h = Harness::load(&root).expect("an empty harness is a statement, not an error");
    assert!(h.is_empty());
    assert!(h.instructions.is_empty());
    assert!(h.persona.is_none());
    assert!(h.skills().is_empty());
    assert!(
        h.skill_catalog().is_none(),
        "no skills must read as `do not register the tool`, not as an empty list"
    );
}

#[test]
fn a_malformed_config_refuses_to_start_and_names_the_file() {
    let root = scratch("malformed").join(".emma");
    write(&root.join("config.json"), "{ not json");
    let err = Harness::load(&root).expect_err("malformed must refuse");
    assert!(
        format!("{err:#}").contains("config.json"),
        "the refusal must name the file. Got: {err:#}"
    );
}

/// `deny_unknown_fields` is the difference between a typo being caught and a
/// hook silently guarding nothing.
#[test]
fn an_unknown_spine_key_is_a_load_error_not_a_shrug() {
    let root = scratch("unknown-key").join(".emma");
    // The persona really exists and the spine is otherwise valid, so the
    // misspelled `persona` key is the *only* thing that can fail this load.
    // Written the obvious way — with no persona directory — the test passes on
    // an unrelated error and stays green with `deny_unknown_fields` removed;
    // that is the shape of green test this lineage has been bitten by before.
    write(
        &root.join("config.json"),
        r#"{"default_persona":"assistant","persona":{"assistant":{}}}"#,
    );
    write(&root.join("personas/assistant/rules.md"), "rules");
    let err = Harness::load(&root).expect_err("unknown keys must fail the load");
    let msg = format!("{err:#}");
    assert!(msg.contains("config.json"), "{msg}");
    assert!(msg.contains("persona"), "the error must name the key: {msg}");
}

/// The same attribute, one level down. A `mathcer` typo would produce a hook
/// with no matcher, silently guarding every tool instead of one.
#[test]
fn an_unknown_hook_key_is_a_load_error_too() {
    let root = scratch("unknown-hook-key").join(".emma");
    std::fs::create_dir_all(root.join("hooks")).expect("mkdir");
    write(
        &root.join("config.json"),
        r#"{"hooks":{"h":{"event":"PreToolUse","command":"hooks/x","mathcer":"Bash"}}}"#,
    );
    let err = Harness::load(&root).expect_err("a misspelled matcher must not be ignored");
    let msg = format!("{err:#}");
    assert!(msg.contains("config.json"), "{msg}");
    // Naming the key is what separates this from the load failing for the
    // unrelated reason that `hooks/x` does not exist — which it also does not.
    assert!(msg.contains("mathcer"), "the error must name the key: {msg}");
}

/// Persona content nothing selects is the silent-drop case: an operator wrote a
/// prompt and Emma would have started without it.
#[test]
fn persona_files_that_nothing_selects_refuse_to_start() {
    let root = scratch("unselected").join(".emma");
    write(&root.join("config.json"), "{}");
    write(&root.join("personas/assistant/rules.md"), "be good");
    let err = Harness::load(&root).expect_err("an unselected persona is an accident");
    assert!(format!("{err:#}").contains("assistant"), "{err:#}");
}

#[test]
fn a_selected_persona_with_no_directory_names_itself() {
    let root = scratch("missing-persona").join(".emma");
    write(&root.join("config.json"), r#"{"default_persona":"ghost"}"#);
    let err = Harness::load(&root).expect_err("a persona with no files must refuse");
    assert!(format!("{err:#}").contains("ghost"), "{err:#}");
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// The property the whole hashing story rests on: one file in means those exact
/// bytes out. If assembly ever trims, re-wraps or normalises, a prompt hash
/// stops being comparable across a refactor and every past action is detached
/// from the configuration that produced it.
#[test]
fn one_file_in_means_those_exact_bytes_out() {
    let text = "# Rules\n\n- one\n-  two   \n\n\ntrailing space   \nend\n";
    let h = Harness::load(one_persona("byte-exact", text)).expect("load");
    assert_eq!(h.instructions, text, "assembly normalised something");
    assert_eq!(h.instructions_hash(), emma_harness::hash::short(text));
    assert!(
        !h.instructions.contains('\r'),
        "CR in the assembled prompt — the hash is now platform-dependent"
    );
}

/// Shared first, and a persona file cannot displace a shared rule.
#[test]
fn assembly_order_is_shared_then_persona() {
    let root = scratch("order").join(".emma");
    write(
        &root.join("config.json"),
        r#"{"default_persona":"assistant","personas":{"assistant":{}}}"#,
    );
    write(&root.join("personas/_shared/rules.md"), "SHARED-RULES");
    write(&root.join("personas/_shared/business.md"), "SHARED-BUSINESS");
    write(&root.join("personas/assistant/rules.md"), "OWN-RULES");
    write(&root.join("personas/assistant/soul.md"), "OWN-SOUL");
    let h = Harness::load(&root).expect("load");
    assert_eq!(
        h.instructions,
        "SHARED-RULES\n\nSHARED-BUSINESS\n\nOWN-RULES\n\nOWN-SOUL"
    );
}

/// An absent layer is skipped, and so is an empty one — an empty file must not
/// contribute a stray separator, because that would move the hash for a file
/// that says nothing.
#[test]
fn absent_and_empty_layers_are_skipped_without_a_stray_separator() {
    let root = scratch("gaps").join(".emma");
    write(
        &root.join("config.json"),
        r#"{"default_persona":"a","personas":{"a":{}}}"#,
    );
    write(&root.join("personas/_shared/rules.md"), "SHARED");
    write(&root.join("personas/_shared/business.md"), "");
    write(&root.join("personas/a/rules.md"), "OWN");
    let h = Harness::load(&root).expect("load");
    assert_eq!(h.instructions, "SHARED\n\nOWN");
}

#[test]
fn shared_is_reserved_and_is_never_a_persona() {
    let root = scratch("shared-reserved").join(".emma");
    write(&root.join("config.json"), "{}");
    write(&root.join("personas/_shared/rules.md"), "SHARED");
    let h = Harness::load(&root).expect("_shared alone must not look like an unselected persona");
    assert!(h.persona.is_none());
    assert!(h.is_empty());
}

// ---------------------------------------------------------------------------
// Skills and commands
// ---------------------------------------------------------------------------

#[test]
fn only_name_and_description_are_needed_to_offer_a_skill() {
    let root = one_persona("skills", "rules");
    with_skill(&root, "marketing-audit", "Run the audit.", "# the long body");
    let h = Harness::load(&root).expect("load");
    let s = h.skills().first().expect("one skill");
    assert_eq!(s.name, "marketing-audit");
    assert_eq!(s.description, "Run the audit.");
    // Everything after the frontmatter, verbatim apart from the leading blank
    // line: a skill body is prompt text and gets the same no-normalising rule as
    // the instructions.
    assert_eq!(s.body, "# the long body\n");
    assert_eq!(
        s.hash,
        emma_harness::hash::short(&s.body),
        "the body hash is what a tool result attributes a load to"
    );
    assert!(
        !h.instructions.contains("the long body"),
        "a skill body must never be always-on prompt text"
    );
}

/// The catalogue rides in the cached prompt prefix, so neither directory order
/// nor the order a persona happened to list them in may decide its bytes.
#[test]
fn the_skill_catalog_is_sorted_and_independent_of_election_order() {
    let root = scratch("catalog").join(".emma");
    write(
        &root.join("config.json"),
        r#"{"default_persona":"a","personas":{"a":{"skills":["zeta","alpha"]}}}"#,
    );
    write(&root.join("personas/a/rules.md"), "rules");
    with_skill(&root, "alpha", "First.", "a");
    with_skill(&root, "zeta", "Last.", "z");
    with_skill(&root, "unelected", "Not chosen.", "u");

    let h = Harness::load(&root).expect("load");
    assert_eq!(h.skill_names(), vec!["alpha", "zeta"]);
    let catalog = h.skill_catalog().expect("two skills");
    assert_eq!(catalog, "\n- `alpha` — First.\n- `zeta` — Last.\n");
    assert!(h.skill("unelected").is_none(), "election is exclusive");
}

#[test]
fn a_persona_electing_a_skill_that_does_not_exist_fails_the_load() {
    let root = scratch("bad-election").join(".emma");
    write(
        &root.join("config.json"),
        r#"{"default_persona":"a","personas":{"a":{"skills":["ghost"]}}}"#,
    );
    write(&root.join("personas/a/rules.md"), "rules");
    let err = Harness::load(&root).expect_err("an unresolvable election is a load error");
    assert!(format!("{err:#}").contains("ghost"), "{err:#}");
}

#[test]
fn commands_expand_at_intake_and_unknown_ones_pass_through() {
    let root = one_persona("commands", "rules");
    write(&root.join("commands/audit.md"), "Run the audit checklist.\n");
    let h = Harness::load(&root).expect("load");

    let e = h.expand_command("/audit acme.com").expect("known command");
    assert_eq!(e.command, "audit");
    assert_eq!(e.raw, "/audit acme.com");
    assert_eq!(e.text, "Run the audit checklist.\n\nacme.com");

    assert!(h.expand_command("/nope").is_none(), "unknown → ordinary text");
    assert!(h.expand_command("audit").is_none());
    assert!(
        h.expand_command("/auditor").is_none(),
        "prefix matching would expand the wrong command"
    );
}

// ---------------------------------------------------------------------------
// The tool allowlist
// ---------------------------------------------------------------------------

#[test]
fn a_persona_naming_an_unregistered_tool_fails_the_load() {
    let root = scratch("bad-tool").join(".emma");
    write(
        &root.join("config.json"),
        r#"{"default_persona":"a","personas":{"a":{"tools":["Bahs"]}}}"#,
    );
    write(&root.join("personas/a/rules.md"), "rules");
    let h = Harness::load(&root).expect("load");
    // `Registry` is not `Debug`, so unwrap the error by hand rather than with
    // `expect_err`.
    let Err(err) = h.select_tools(registry(&["Read", "Bash"])) else {
        panic!("a tool that does not exist must fail before any call");
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("Bahs"), "{msg}");
    assert!(
        msg.contains("Bash"),
        "the error must list what is registered, or a typo is a scavenger hunt: {msg}"
    );
}

/// The change from tustle-agent, and the reason for it. There this field
/// asserted and filtered nothing, which was survivable because every tool was a
/// read-only search. Emma runs `Bash` and `Write`.
#[test]
fn the_allowlist_actually_removes_tools_from_the_registry() {
    let root = scratch("allowlist").join(".emma");
    write(
        &root.join("config.json"),
        r#"{"default_persona":"a","personas":{"a":{"tools":["Read","Grep"]}}}"#,
    );
    write(&root.join("personas/a/rules.md"), "rules");
    let h = Harness::load(&root).expect("load");

    let selected = h
        .select_tools(registry(&["Read", "Write", "Bash", "Grep"]))
        .expect("every named tool is registered");
    assert_eq!(
        selected.names(),
        vec!["Read", "Grep"],
        "an operator who allowlists two read-only tools must not get a shell"
    );
    assert!(
        selected.get("Bash").is_none(),
        "the tool that can destroy the working tree is exactly the one this must remove"
    );
}

/// Registry order, not allowlist order: the tool schema rides in the cached
/// prompt prefix, so the bytes must not depend on how the operator typed it.
#[test]
fn selection_keeps_registry_order_so_the_prompt_prefix_is_stable() {
    let root = scratch("allowlist-order").join(".emma");
    write(
        &root.join("config.json"),
        r#"{"default_persona":"a","personas":{"a":{"tools":["Grep","Read"]}}}"#,
    );
    write(&root.join("personas/a/rules.md"), "rules");
    let h = Harness::load(&root).expect("load");
    let selected = h.select_tools(registry(&["Read", "Grep"])).expect("select");
    assert_eq!(selected.names(), vec!["Read", "Grep"]);
}

#[test]
fn no_allowlist_means_every_registered_tool() {
    let h = Harness::load(one_persona("no-allowlist", "rules")).expect("load");
    assert!(h.tools().is_none());
    let selected = h.select_tools(registry(&["Read", "Bash"])).expect("select");
    assert_eq!(selected.names(), vec!["Read", "Bash"]);
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

#[test]
fn the_snapshot_reports_identity_and_never_prompt_text() {
    let root = one_persona("snapshot", "SECRET-PROMPT-TEXT");
    with_skill(&root, "s", "d", "SECRET-SKILL-BODY");
    let h = Harness::load(&root).expect("load");
    let snap = h.snapshot().to_string();
    assert!(!snap.contains("SECRET-PROMPT-TEXT"), "{snap}");
    assert!(!snap.contains("SECRET-SKILL-BODY"), "{snap}");
    assert!(snap.contains(&h.instructions_hash()));
    assert_eq!(h.snapshot()["flavor"], "emma");
    assert_eq!(Flavor::of(&h.root), Flavor::Emma);
}
