//! `agents/<name>.md` read as delegation targets, against real files.
//!
//! **The library this was measured on is not a fixture.** A machine with Claude
//! Code on it has `~/.claude/agents/`, and the one this was written against has
//! ninety files in it, accumulated over months for a different program. A parser
//! tested only against files its author wrote agrees with its author's model of
//! the world, which is the lesson this repository has already paid for once.
//!
//! So the last test in this file copies whatever is in `~/.claude/agents/` into
//! a temporary harness, loads it, and reports what happened. It is skipped when
//! that directory is not there — a test that silently passes on a machine with
//! no library would be worse than no test, so it says which it did.
//!
//! Everything above it is the specific rulings, written as fixtures because they
//! are about cases a library may or may not happen to contain.

mod support;

use std::path::{Path, PathBuf};

use emma_harness::{Flavor, Harness};

/// Names the scratch directories these tests leave behind, so a failure can be
/// looked at on disk.
const TAG: &str = "agent-types";

/// A `.claude/` harness containing only `agents/`, which is the shape that
/// isolates the agent loader from settings, skills and commands.
fn with_agents(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
    let root = dir.join(".claude");
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    for (name, body) in files {
        std::fs::write(agents.join(format!("{name}.md")), body).unwrap();
    }
    root
}

fn load(root: &Path) -> Harness {
    // `load_selecting` rather than `load`: the latter reads `EMMA_PERSONA` from
    // the process environment, and a test whose result depends on the
    // developer's shell passes for the wrong reason.
    Harness::load_selecting(root, Flavor::Claude, None).unwrap()
}

// region: The fields, and who reads each one
// ---------------------------------------------------------------------------
// The fields, and who reads each one
// ---------------------------------------------------------------------------

#[test]
fn every_field_a_real_agent_file_carries_is_read_or_ignored_on_purpose() {
    // Written from a real file, `category` and all. `category` and `version` are
    // Claude Code's and Emma has no opinion about them — the frontmatter struct
    // must never gain `deny_unknown_fields`, because on the library this was
    // measured against every one of those keys would have been a boot failure.
    let root = with_agents(
        &support::scratch(TAG),
        &[(
            "admin-dashboard-specialist",
            "---\n\
             name: admin-dashboard-specialist\n\
             description: Use proactively for creating admin dashboards.\n\
             category: general\n\
             model: claude-sonnet-4-5\n\
             version: 1.0.0\n\
             tools:\n\
             - Read\n\
             - Write\n\
             - Glob\n\
             ---\n\
             You build admin dashboards.\n",
        )],
    );
    let harness = load(&root);
    let types = harness.agent_types();
    assert_eq!(types.len(), 1, "{:?}", harness.agent_notes());
    let ty = &types[0];
    assert_eq!(ty.name, "admin-dashboard-specialist");
    assert!(ty.description.contains("admin dashboards"));
    assert_eq!(
        ty.tools.as_deref(),
        Some(["Read", "Write", "Glob"].map(String::from).as_slice())
    );
    // Honoured rather than ignored. A file saying `model: claude-sonnet-4-5`
    // that quietly runs on something else is a lie the user cannot see.
    assert_eq!(ty.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(ty.instructions.trim(), "You build admin dashboards.");
    assert!(
        harness.agent_notes().is_empty(),
        "{:?}",
        harness.agent_notes()
    );
}

#[test]
fn tools_may_be_a_list_or_one_comma_separated_string() {
    // Both are common in the wild — five files of the ninety use the second —
    // and accepting one while silently ignoring the other produces an empty
    // allowlist, which under Emma's rules means no tools at all.
    let root = with_agents(
        &support::scratch(TAG),
        &[(
            "backend-implementer",
            "---\ndescription: Backend work.\ntools: Read, Edit, Write, Bash\n---\nbody\n",
        )],
    );
    assert_eq!(
        load(&root).agent_types()[0].tools.as_deref(),
        Some(
            ["Read", "Edit", "Write", "Bash"]
                .map(String::from)
                .as_slice()
        )
    );
}

/// **The trap in this format, and it is live.**
///
/// `tools:` with nothing after it, `tools: []` and `tools: ""` are three
/// spellings of the same thing, and under Emma's rules an empty allowlist means
/// *no tools at all* — an agent that silently cannot do anything, failing in a
/// way that looks like the model being bad rather than the config being dropped.
/// In Claude Code an absent `tools` means inherit everything, and nobody writes
/// an empty one meaning "this agent may do nothing".
#[test]
fn an_empty_tools_list_means_inherit_and_never_means_nothing() {
    let root = with_agents(
        &support::scratch(TAG),
        &[
            ("null", "---\ndescription: d\ntools:\n---\nbody\n"),
            ("empty-list", "---\ndescription: d\ntools: []\n---\nbody\n"),
            (
                "empty-string",
                "---\ndescription: d\ntools: \"\"\n---\nbody\n",
            ),
            ("absent", "---\ndescription: d\n---\nbody\n"),
        ],
    );
    for ty in load(&root).agent_types() {
        assert_eq!(
            ty.tools, None,
            "`{}` resolved to an allowlist rather than to inherit-everything",
            ty.name
        );
    }
}

#[test]
fn the_file_name_is_the_name_and_a_disagreement_is_said_out_loud() {
    // The stem is what `EMMA_PERSONA` names and the only one of the two
    // guaranteed unique — two files may declare the same `name`, and a catalogue
    // keyed on a value that can collide silently drops one of them.
    let root = with_agents(
        &support::scratch(TAG),
        &[(
            "on-disk",
            "---\nname: in-frontmatter\ndescription: d\n---\nbody\n",
        )],
    );
    let harness = load(&root);
    assert_eq!(harness.agent_types()[0].name, "on-disk");
    let notes = harness.agent_notes().join("\n");
    assert!(notes.contains("in-frontmatter"), "{notes}");
    assert!(notes.contains("file name wins"), "{notes}");
}

#[test]
fn an_agent_with_no_description_is_dropped_by_name_rather_than_silently() {
    // Four of the ninety are like this. `description` is what a calling model
    // reads to choose, so a file without one cannot be a delegation target —
    // but a catalogue quietly shorter than the directory is the gap nobody
    // notices until the model cannot find an agent that is plainly there.
    let root = with_agents(
        &support::scratch(TAG),
        &[
            (
                "research-analyst",
                "---\nname: research-analyst\n---\nbody\n",
            ),
            ("usable", "---\ndescription: d\n---\nbody\n"),
        ],
    );
    let harness = load(&root);
    assert_eq!(harness.agent_types().len(), 1);
    let notes = harness.agent_notes().join("\n");
    assert!(notes.contains("research-analyst"), "{notes}");
    assert!(notes.contains("description"), "{notes}");
}

#[test]
fn a_directory_with_no_agents_is_not_an_error() {
    // The ordinary case for a project that has never wanted delegation, and the
    // boot must not care.
    let root = support::scratch(TAG).join(".claude");
    std::fs::create_dir_all(&root).unwrap();
    let harness = load(&root);
    assert!(harness.agent_types().is_empty());
    assert!(harness.agent_notes().is_empty());
}

#[test]
fn an_agent_file_is_still_selectable_as_a_persona() {
    // One file format, two selection rules. The delegation catalogue must not
    // have broken the older use: `EMMA_PERSONA` still picks one of these as the
    // run's own prompt, and its `tools` is still the run's allowlist.
    let root = with_agents(
        &support::scratch(TAG),
        &[(
            "explorer",
            "---\ndescription: d\ntools: Read, Grep\n---\nI explore.\n",
        )],
    );
    let harness = Harness::load_selecting(&root, Flavor::Claude, Some("explorer".into())).unwrap();
    assert_eq!(harness.persona.as_deref(), Some("explorer"));
    assert!(harness.instructions.contains("I explore."));
    assert_eq!(
        harness.tools(),
        Some(["Read", "Grep"].map(String::from).as_slice())
    );
    // …and it is offered for delegation as well, which is the second,
    // independent use of the same bytes.
    assert_eq!(harness.agent_types().len(), 1);
}

// endregion: The fields, and who reads each one

// region: The real library
// ---------------------------------------------------------------------------
// The real library
//
// Ninety files nobody here wrote. A fake agrees with your model of the world.
// ---------------------------------------------------------------------------

fn user_agents() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    let dir = home.join(".claude").join("agents");
    dir.is_dir().then_some(dir)
}

/// Load a real `~/.claude/agents/` library and report what came of it.
///
/// It asserts two things and reports the rest, because the library is somebody's
/// and its contents will change: that the load does not fail, and that the great
/// majority of files end up usable. A parser that dropped half of them would
/// pass a "does not crash" test and be useless.
///
/// Run it with `--nocapture` to see the breakdown.
#[test]
fn a_real_agent_library_loads_and_most_of_it_is_usable() {
    let Some(source) = user_agents() else {
        eprintln!("skipped: no ~/.claude/agents on this machine, so there was no real library");
        return;
    };
    let root = support::scratch(TAG).join(".claude");
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    // **No `unwrap` on the source.** `~/.claude/agents` is a live directory
    // outside this repository and a file can vanish between the listing and the
    // copy; the same race is written up at length beside the skill corpus in
    // `claude_compat.rs`. Counted and printed rather than dropped, so a library
    // that stops being copyable altogether cannot look like one that copied
    // cleanly.
    let mut copied = 0usize;
    let mut vanished = 0usize;
    let Ok(listing) = std::fs::read_dir(&source) else {
        eprintln!("skipped: {} could not be listed", source.display());
        return;
    };
    for entry in listing.flatten() {
        let path = entry.path();
        // Top-level `*.md` only, which is what the loader reads. A real library
        // has subdirectories under `agents/`; walking into them would flatten
        // two namespaces into one where a collision is resolved by directory
        // iteration order.
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        if std::fs::copy(&path, agents.join(name)).is_err() {
            vanished += 1;
            continue;
        }
        copied += 1;
    }
    if vanished > 0 {
        eprintln!(
            "{vanished} agent file(s) changed under the copy and were left out; \
             the bar below is applied to the {copied} that were read"
        );
    }
    if copied == 0 {
        eprintln!("skipped: ~/.claude/agents has no top-level .md files");
        return;
    }

    let harness = load(&root);
    let types = harness.agent_types();
    let inheriting = types.iter().filter(|t| t.tools.is_none()).count();
    let with_model = types.iter().filter(|t| t.model.is_some()).count();
    eprintln!("real library: {copied} files in {}", source.display());
    eprintln!("  offered for delegation: {}", types.len());
    eprintln!("  of those, inheriting the caller's tools: {inheriting}");
    eprintln!("  of those, naming their own model: {with_model}");
    eprintln!("  not offered, or noted:");
    for note in harness.agent_notes() {
        eprintln!("    {note}");
    }

    // Not "some parsed". A parser that kept a third of a real library would be
    // a compatibility layer in name only.
    assert!(
        types.len() * 10 >= copied * 8,
        "only {} of {copied} real agent files became delegation targets",
        types.len()
    );
    // Every one of them can actually be chosen: a name and a description are
    // what the closed enum and the catalogue are built from.
    for ty in types {
        assert!(!ty.name.trim().is_empty());
        assert!(!ty.description.trim().is_empty(), "{}", ty.name);
        // The trap, checked against the real thing rather than against a
        // fixture: nothing in a real library resolves to "no tools at all".
        assert_ne!(
            ty.tools.as_deref(),
            Some(&[][..]),
            "`{}` resolved to an empty allowlist",
            ty.name
        );
    }
}

// endregion: The real library
