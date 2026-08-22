//! Reading permission rules off disk: which files, which scopes, and the one
//! place Emma deliberately reads less than Claude Code does.
//!
//! The harness does not understand a rule — `emma::permissions` owns the syntax
//! and the matcher, and tests it there. What is under test here is the *reading*:
//! that the two project files merge rather than override, that both directory
//! flavours carry the same block, that a broken `settings.local.json` refuses the
//! boot, and — the one with teeth — that an `allow` rule in the user's global
//! `~/.claude/settings.json` does **not** cross into Emma.
//!
//! That last one is not a style preference. The owner's real
//! `~/.claude/settings.json` carries `Bash(rm:*)`, `Bash(bash:*)` and
//! `"defaultMode": "dontAsk"`. Honouring user-scope `allow` would have handed
//! Emma an unprompted shell in every repository on the machine, granted by
//! nobody, on the first run after the feature shipped. `deny` crosses because it
//! can only ever remove a capability.

mod support;

use emma_harness::{Flavor, Harness, PermissionKind};
use support::*;

/// Every rule the harness resolved, as `"kind rule"`, in the order it produced
/// them. Comparing strings rather than structs keeps a failure readable.
fn rules(h: &Harness) -> Vec<String> {
    h.permissions()
        .iter()
        .map(|p| format!("{} {}", p.kind.word(), p.rule))
        .collect()
}

#[test]
fn the_spine_and_the_local_file_merge_rather_than_override() {
    // Claude Code's own rule: permission lists "merge across scopes rather than
    // override". It is safe to merge precisely because `deny` beats `allow` at
    // match time, so a restrictive rule in either file survives a permissive one
    // in the other. If this ever became "nearest wins", the project's committed
    // `deny` list would be silently deleted by one developer's local grant.
    let root = scratch("perm-merge").join(".claude");
    write(
        &root.join("settings.json"),
        r#"{"permissions":{"deny":["WebFetch(domain:evil.example)"],
                           "allow":["WebSearch"]}}"#,
    );
    write(
        &root.join("settings.local.json"),
        r#"{"permissions":{"allow":["WebFetch(domain:apnews.com)"]}}"#,
    );

    let h = Harness::load(&root).expect("load");
    assert_eq!(
        rules(&h),
        vec![
            "deny WebFetch(domain:evil.example)",
            "allow WebSearch",
            "allow WebFetch(domain:apnews.com)",
        ]
    );
    // Each rule carries the file it came from, because "which file" is one of
    // the two sentences an operator needs when a rule does not fire, and a
    // merged `Vec<String>` cannot answer it.
    let sources: Vec<_> = h
        .permissions()
        .iter()
        .map(|p| p.source.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        sources,
        vec!["settings.json", "settings.json", "settings.local.json"]
    );
}

#[test]
fn six_remembered_grants_in_one_local_file_all_come_back() {
    // The reading half of the multi-run claim in `emma/tests/permissions.rs`.
    // That file proves six separate runs *write* six rules into one document;
    // this proves the harness then hands all six back — none swallowed by
    // another, none lost to the keys around them, and the project's own rules
    // still in front of them.
    //
    // Six rather than one because the failure mode being guarded is a reader
    // that keeps the last rule it saw, or the first, and one rule cannot tell
    // those two apart from a reader that works.
    let hosts = [
        "example.com",
        "example.org",
        "example.net",
        "www.iana.org",
        "www.rust-lang.org",
        "docs.rs",
    ];
    let root = scratch("perm-six").join(".claude");
    write(
        &root.join("settings.json"),
        r#"{"permissions":{"deny":["WebFetch(domain:evil.example)"]}}"#,
    );
    // The shape `permissions::remember` leaves behind after six grants, with
    // the unrelated keys a real file has around them.
    let allow: Vec<String> = hosts
        .iter()
        .map(|h| format!("WebFetch(domain:{h})"))
        .collect();
    write(
        &root.join("settings.local.json"),
        &serde_json::json!({
            "statusLine": { "type": "command", "command": "hooks/status" },
            "env": { "EMMA_TEST": "1" },
            "permissions": { "allow": allow }
        })
        .to_string(),
    );

    let h = Harness::load(&root).expect("load");
    let mut want = vec!["deny WebFetch(domain:evil.example)".to_string()];
    want.extend(hosts.iter().map(|h| format!("allow WebFetch(domain:{h})")));
    assert_eq!(rules(&h), want, "six grants did not survive the read");
    // Every one of them names the file it came from, which is the sentence an
    // operator needs when the sixth rule is the one not firing.
    for entry in h.permissions().iter().skip(1) {
        assert!(
            entry.source.ends_with("settings.local.json"),
            "{:?}",
            entry.source
        );
    }
}

#[test]
fn a_dot_emma_project_carries_the_same_block_and_writes_beside_itself() {
    // The block is Claude Code's shape in both directories, so a rule does not
    // have to be re-typed to move between them. And the file a remembered grant
    // lands in sits beside whichever harness actually loaded — a project that
    // deliberately chose `.emma/` must not acquire a `.claude/` directory
    // because somebody answered a prompt.
    let root = scratch("perm-emma").join(".emma");
    write(
        &root.join("config.json"),
        r#"{"permissions":{"ask":["Bash"],"allow":["WebSearch"]}}"#,
    );
    let h = Harness::load(&root).expect("load");
    assert_eq!(h.flavor, Flavor::Emma);
    assert_eq!(rules(&h), vec!["ask Bash", "allow WebSearch"]);
    assert_eq!(h.permissions_file(), root.join("settings.local.json"));
}

#[test]
fn a_permissions_block_does_not_stop_the_rest_of_the_file_being_read() {
    // The keys around it are the ones a real Claude Code file has and Emma has
    // no opinion about. Denying unknown fields here would refuse to boot in
    // essentially every working repository — the ruling `claude.rs` makes twice
    // already, applied to the block this feature added.
    let root = scratch("perm-permissive").join(".claude");
    write(
        &root.join("settings.json"),
        r#"{"model":"claude-opus-4","env":{"FOO":"1"},
            "permissions":{"allow":["WebSearch"],"defaultMode":"dontAsk",
                           "additionalDirectories":["/tmp"]}}"#,
    );
    let h = Harness::load(&root).expect("a real settings.json failed the boot");
    assert_eq!(rules(&h), vec!["allow WebSearch"]);
}

#[test]
fn a_malformed_local_settings_file_refuses_the_boot_and_names_itself() {
    // Different from a rule Emma cannot evaluate, which is only a note. An
    // unparseable file says *nothing at all*, and booting past it would mean
    // running with a `deny` list the operator believes is in force. Same ruling
    // the crate makes for every other malformed config.
    let root = scratch("perm-broken").join(".claude");
    write(&root.join("settings.json"), "{}");
    write(&root.join("settings.local.json"), "{ not json");
    let err = Harness::load(&root)
        .expect_err("a malformed settings.local.json booted")
        .to_string();
    assert!(err.contains("settings.local.json"), "{err}");
    assert!(err.contains("malformed"), "{err}");
}

#[test]
fn an_absent_or_empty_permissions_block_grants_nothing() {
    // The empty case has to be empty. A reader that turned "no rules" into any
    // rule at all would be the entire gate switched off by a file that says
    // nothing.
    for body in ["{}", r#"{"permissions":{}}"#, ""] {
        let root = scratch("perm-empty").join(".claude");
        write(&root.join("settings.json"), body);
        let h = Harness::load(&root).expect("load");
        assert!(h.permissions().is_empty(), "`{body}` produced a rule");
    }
}

#[test]
fn the_user_scope_contributes_deny_and_never_allow() {
    // The divergence, with the owner's real file as the fixture. If this
    // inverts, `Bash(rm:*)` from a global settings file becomes a standing grant
    // in every repository on the machine — granted by nobody, for a program that
    // is not running.
    let home = scratch("perm-user");
    write(
        &home.join(".claude").join("settings.json"),
        r#"{"permissions":{
             "allow":["Bash(rm:*)","Bash(bash:*)","Glob","Grep"],
             "ask":["Write"],
             "deny":["WebFetch(domain:evil.example)"]},
           "defaultMode":"dontAsk"}"#,
    );
    let (entries, _notes) =
        emma_harness::user_permissions(Some(&home)).expect("read user settings");
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].kind, PermissionKind::Deny);
    assert_eq!(entries[0].rule, "WebFetch(domain:evil.example)");
    assert!(entries[0].source.ends_with("settings.json"));
}

#[test]
fn a_missing_or_unreadable_user_settings_file_is_not_a_failure() {
    // The only thing taken from it is restrictions, so failing to read it can
    // only leave Emma asking more often. That is not worth refusing to start
    // over — and the file belongs to another program.
    assert!(emma_harness::user_permissions(None).unwrap().0.is_empty());
    let home = scratch("perm-user-none");
    let (entries, notes) = emma_harness::user_permissions(Some(&home)).unwrap();
    assert!(entries.is_empty());
    assert!(
        notes.is_empty(),
        "an absent file is not a problem: {notes:?}"
    );

    // A file that cannot be parsed still must not refuse the boot — but it must
    // never be quiet either. Until this assertion existed, one trailing comma
    // discarded the operator's entire deny list and `config check` reported
    // "(none)", which reads as "you wrote no rules" rather than "yours could not
    // be read". The empty-entries half of this test passed throughout.
    write(&home.join(".claude").join("settings.json"), "{ not json");
    let (entries, notes) = emma_harness::user_permissions(Some(&home)).unwrap();
    assert!(entries.is_empty(), "a broken file grants nothing");
    assert_eq!(notes.len(), 1, "a broken file must be reported: {notes:?}");
    assert!(
        notes[0].contains("settings.json") && notes[0].contains("deny"),
        "the note must name the file and what was lost: {}",
        notes[0]
    );

    // The other half of the same rule, and the one an independent review caught
    // after the parse path was fixed: a settings file that cannot be *read* --
    // a directory where a file was expected, a bad ACL, an I/O fault -- must
    // also fail open with a note rather than refusing the boot. Until this
    // assertion existed the read path still carried a `?`, so an unreadable file
    // took Emma down over a file belonging to another program entirely.
    // A directory where the file should be. `read_if_present` asks `is_file()`
    // first, so this is the *absent* case, not the unreadable one: no entries, no
    // note, and above all no refusal to start.
    //
    // **What this does not cover, stated rather than implied:** a file that
    // exists and cannot be read — a bad ACL, a locked handle, an I/O fault. That
    // path now returns a note instead of `?`, and no portable test drives it;
    // forcing it needs `icacls` on Windows or a `chmod 000` on unix, neither of
    // which belongs in a cross-platform suite. An earlier draft of this test
    // asserted `notes.is_empty() || notes[0].contains(..)`, which passes either
    // way and proves nothing — the false receipt this repository keeps paying
    // for. It is better to name the gap than to hold a receipt for it.
    let home = scratch("perm-user-unreadable");
    let settings = home.join(".claude").join("settings.json");
    std::fs::create_dir_all(&settings).expect("a directory standing in for the file");
    let (entries, notes) = emma_harness::user_permissions(Some(&home))
        .expect("a settings path that is not a file must never refuse the boot");
    assert!(entries.is_empty(), "an absent file grants nothing");
    assert!(
        notes.is_empty(),
        "a path that is not a file is the absent case, not a problem to report: {notes:?}"
    );
}

#[test]
fn the_snapshot_shows_every_rule_and_where_it_came_from() {
    // `emma config check` prints this. The whole persisted grant rests on the
    // user being able to see what they granted, and this is the machine-readable
    // half of that.
    let root = scratch("perm-snapshot").join(".claude");
    write(
        &root.join("settings.json"),
        r#"{"permissions":{"allow":["WebFetch(domain:apnews.com)"]}}"#,
    );
    let snap = Harness::load(&root).expect("load").snapshot();
    let listed = snap["permissions"].as_array().expect("permissions array");
    assert_eq!(listed.len(), 1, "{snap}");
    assert_eq!(listed[0]["rule"], "WebFetch(domain:apnews.com)");
    assert_eq!(listed[0]["kind"], "allow");
    assert!(listed[0]["source"]
        .as_str()
        .unwrap()
        .ends_with("settings.json"));
}
