//! The wiki store, tested against tempdirs. Nothing here touches the real
//! repository's `.emma/` or the real `~/.emma`: every `Wiki` in this file is
//! rooted in a `TempDir` that dies with the test.
//!
//! **These run against a real filesystem on purpose.** Every guarantee this
//! module makes is a claim about files — that a slug cannot collide with an
//! archived one, that a rename moved rather than copied, that a page written
//! on one machine parses on another. A fake filesystem agrees with whoever
//! wrote it about all three.

use super::*;
use tempfile::TempDir;

/// A wiki in a throwaway directory, laid out exactly as `Wiki::project` would
/// lay one out inside a repository.
fn wiki() -> (TempDir, Wiki) {
    let dir = TempDir::new().expect("tempdir");
    let wiki = Wiki::project(dir.path()).expect("open");
    (dir, wiki)
}

fn seed(w: &Wiki, title: &str, category: Category) -> Page {
    w.create(title, category, "session test", "The body.")
        .expect("create")
}

fn slugs(l: &Listing) -> Vec<String> {
    l.pages.iter().map(|p| p.slug.clone()).collect()
}

#[test]
fn opening_a_wiki_lays_out_the_pattern_and_installs_the_schema() {
    let (dir, w) = wiki();
    let root = dir.path().join(".emma/memory");
    assert_eq!(w.root(), root, "the root is the parameter, never a global");
    assert!(root.join("pages").is_dir(), "pages/ missing");
    assert!(root.join("archive").is_dir(), "archive/ missing");
    assert!(root.join("index.md").is_file(), "index.md missing");
    assert!(root.join("log.md").is_file(), "log.md missing");
    let schema = std::fs::read_to_string(root.join("schema.md")).expect("schema.md");
    assert!(
        schema.contains("CONTRADICTION"),
        "the built-in schema must teach the contradiction marker"
    );
    assert!(
        schema.contains("Never write a secret"),
        "the built-in schema must say these files are plaintext and committable"
    );
}

#[test]
fn a_users_edited_schema_is_never_overwritten_by_a_later_open() {
    let (dir, _w) = wiki();
    let schema = dir.path().join(".emma/memory/schema.md");
    std::fs::write(&schema, "mine now").unwrap();
    let _reopened = Wiki::project(dir.path()).expect("reopen");
    assert_eq!(std::fs::read_to_string(&schema).unwrap(), "mine now");
}

#[test]
fn a_reopen_does_not_truncate_the_log_a_session_already_wrote() {
    let (dir, w) = wiki();
    seed(&w, "Alpha", Category::Facts);
    let _reopened = Wiki::project(dir.path()).expect("reopen");
    let log = std::fs::read_to_string(dir.path().join(".emma/memory/log.md")).unwrap();
    assert!(log.contains("create | alpha"), "the log was reset:\n{log}");
}

#[test]
fn the_root_is_a_parameter_so_the_global_wiki_is_the_same_code() {
    let dir = TempDir::new().unwrap();
    let w = Wiki::global(dir.path()).expect("open global");
    assert_eq!(w.root(), dir.path().join(".emma/memory"));
    assert!(w.root().join("pages").is_dir());
}

#[test]
fn a_title_becomes_a_slug_and_a_collision_gets_a_number() {
    let (_d, w) = wiki();
    let a = seed(&w, "Owner prefers tabs", Category::Preferences);
    assert_eq!(a.slug, "owner-prefers-tabs");
    let b = seed(&w, "Owner prefers tabs!", Category::Preferences);
    assert_eq!(b.slug, "owner-prefers-tabs-2", "a collision is numbered");
    let c = seed(&w, "Owner  prefers — tabs", Category::Preferences);
    assert_eq!(c.slug, "owner-prefers-tabs-3");
    assert_eq!(w.list(Filter::default()).unwrap().pages.len(), 3);
}

/// The guarantee: `create` numbers against **both** directories. Archive the
/// only page with a slug, then create a page whose title produces that same
/// slug. A store that checked `pages/` alone would hand the new page the free
/// name — and `read` searches `pages/` first, so the archived memory would
/// become unreachable under a title that is not its own.
#[test]
fn a_new_memory_never_takes_the_slug_of_an_archived_one() {
    let (dir, w) = wiki();
    let old = seed(&w, "Deploy target", Category::Projects);
    w.archive(&old.slug).unwrap();
    assert!(!dir
        .path()
        .join(".emma/memory/pages/deploy-target.md")
        .exists());

    let new = w
        .create(
            "Deploy target",
            Category::Projects,
            "session two",
            "Now int.",
        )
        .unwrap();
    assert_eq!(
        new.slug, "deploy-target-2",
        "the archive was not consulted for the free slug"
    );
    let archived = w.read("deploy-target").unwrap();
    assert!(archived.archived);
    assert!(
        archived.body.contains("The body."),
        "the archived memory was overwritten: {}",
        archived.body
    );
}

#[test]
fn a_page_round_trips_through_disk_with_its_wiki_links_intact() {
    let (_d, w) = wiki();
    let body = "Emma reads [[index-first]] before [[any-page]].\n\n- a list\n";
    let made = w
        .create("Index first", Category::Workflows, "session 0f2a", body)
        .unwrap();
    let read = w.read(&made.slug).unwrap();
    assert_eq!(read.title, "Index first");
    assert_eq!(read.front.category, Category::Workflows);
    assert!(!read.front.pinned);
    assert_eq!(read.front.source, "session 0f2a");
    assert_eq!(read.front.created.len(), 10, "created is an ISO date");
    assert!(
        read.body.contains("[[index-first]]") && read.body.contains("[[any-page]]"),
        "wiki-links must survive the round trip: {}",
        read.body
    );
    assert!(read.body.contains("- a list"));
}

#[test]
fn pin_and_unpin_move_only_the_flag() {
    let (_d, w) = wiki();
    let p = seed(&w, "Deploy runbook", Category::Workflows);
    let created = p.front.created.clone();
    let pinned = w.pin(&p.slug).unwrap();
    assert!(pinned.front.pinned);
    assert_eq!(pinned.front.created, created, "pinning is not a rebirth");
    assert_eq!(pinned.body, p.body);
    assert!(!w.unpin(&p.slug).unwrap().front.pinned);
}

#[test]
fn update_rewrites_the_body_and_keeps_the_birthday_the_slug_and_the_pin() {
    let (_d, w) = wiki();
    let p = w
        .create("Deploy target", Category::Projects, "session one", "Dev.")
        .unwrap();
    w.pin(&p.slug).unwrap();

    let after = w
        .update(
            &p.slug,
            "Int since 2026-08-27. See [[deploy-runbook]].",
            Some("session two"),
        )
        .unwrap();

    assert_eq!(
        after.slug, p.slug,
        "a slug is a link target and does not move"
    );
    assert_eq!(
        after.front.created, p.front.created,
        "created is a birthday"
    );
    assert!(after.front.pinned, "an update is not an unpin");
    assert_eq!(after.front.source, "session two");
    assert_eq!(
        after.title, "Deploy target",
        "the heading survives a body-only edit"
    );
    assert!(after.body.contains("Int since 2026-08-27"));
    assert!(
        !after.body.contains("Dev."),
        "the old body is gone: {}",
        after.body
    );

    // Reading it back off disk agrees, which is the half a returned struct
    // cannot prove.
    assert_eq!(w.read(&p.slug).unwrap(), after);
}

#[test]
fn update_leaves_the_source_alone_when_the_caller_does_not_name_a_new_one() {
    let (_d, w) = wiki();
    let p = w
        .create("Fact", Category::Facts, "session one", "Old.")
        .unwrap();
    let after = w.update(&p.slug, "New.", None).unwrap();
    assert_eq!(after.front.source, "session one");
}

#[test]
fn update_renames_a_page_when_the_new_body_carries_a_new_heading() {
    let (_d, w) = wiki();
    let p = seed(&w, "Old name", Category::Facts);
    let after = w.update(&p.slug, "# New name\n\nBody.", None).unwrap();
    assert_eq!(after.title, "New name");
    assert_eq!(after.slug, "old-name", "renaming is not re-slugging");
}

#[test]
fn an_archived_memory_is_history_and_update_says_so_rather_than_rewriting_it() {
    let (_d, w) = wiki();
    let p = seed(&w, "Stale", Category::Facts);
    w.archive(&p.slug).unwrap();
    let e = w.update(&p.slug, "resurrected", None).unwrap_err();
    assert!(e.to_string().contains("archived"), "{e}");
    assert!(w.read(&p.slug).unwrap().body.contains("The body."));
}

#[test]
fn list_filters_by_category_and_by_pin_and_excludes_the_archive_by_default() {
    let (_d, w) = wiki();
    let a = seed(&w, "Alpha", Category::Facts);
    let b = seed(&w, "Beta", Category::People);
    let c = seed(&w, "Gamma", Category::Facts);
    w.pin(&a.slug).unwrap();
    w.archive(&c.slug).unwrap();

    let all = w.list(Filter::default()).unwrap();
    assert_eq!(slugs(&all), vec!["alpha", "beta"], "archived is excluded");

    let facts = w.list(Filter::default().category(Category::Facts)).unwrap();
    assert_eq!(slugs(&facts), vec!["alpha"]);

    let pinned = w.list(Filter::default().pinned(true)).unwrap();
    assert_eq!(slugs(&pinned), vec!["alpha"]);

    let unpinned = w.list(Filter::default().pinned(false)).unwrap();
    assert_eq!(slugs(&unpinned), vec!["beta"]);

    let with_archive = w
        .list(Filter::default().archived(Archived::Included))
        .unwrap();
    assert_eq!(slugs(&with_archive), vec!["alpha", "beta", "gamma"]);

    let only = w.list(Filter::default().archived(Archived::Only)).unwrap();
    assert_eq!(slugs(&only), vec!["gamma"]);
    assert_eq!(b.front.category, Category::People);
}

#[test]
fn archiving_moves_the_whole_file_and_never_deletes_it() {
    let (dir, w) = wiki();
    let p = seed(&w, "Stale claim", Category::Projects);
    let before =
        std::fs::read_to_string(dir.path().join(".emma/memory/pages/stale-claim.md")).unwrap();
    w.archive(&p.slug).unwrap();
    assert!(!dir
        .path()
        .join(".emma/memory/pages/stale-claim.md")
        .exists());
    let after =
        std::fs::read_to_string(dir.path().join(".emma/memory/archive/stale-claim.md")).unwrap();
    assert_eq!(before, after, "the file moves whole");
    let read = w.read(&p.slug).unwrap();
    assert!(read.archived, "read finds it in the archive and says so");
}

#[test]
fn archiving_a_slug_that_is_not_live_says_so_rather_than_half_succeeding() {
    let (_d, w) = wiki();
    let e = w.archive("never-existed").unwrap_err();
    assert!(e.to_string().contains("never-existed"), "{e}");
}

#[test]
fn counts_are_per_category_and_report_pinned_and_archived_separately() {
    let (_d, w) = wiki();
    let a = seed(&w, "One", Category::Facts);
    seed(&w, "Two", Category::Facts);
    let c = seed(&w, "Three", Category::People);
    w.pin(&a.slug).unwrap();
    w.archive(&c.slug).unwrap();
    let counts = w.counts().unwrap();
    assert_eq!(counts.of(Category::Facts), 2);
    assert_eq!(
        counts.of(Category::People),
        0,
        "archived pages leave the live counts"
    );
    assert_eq!(counts.of(Category::References), 0);
    assert_eq!(counts.total, 2);
    assert_eq!(counts.pinned, 1);
    assert_eq!(counts.archived, 1);
    assert_eq!(counts.malformed, 0);
}

/// The invariant the pattern is built on: the index is not a cache that
/// drifts. Mutate through every mutating call there is, then rebuild the index
/// from `pages/` — the ground truth — and demand the bytes are identical.
#[test]
fn every_mutation_leaves_the_index_exactly_what_a_rebuild_would_write() {
    let (dir, w) = wiki();
    let index = dir.path().join(".emma/memory/index.md");

    let a = seed(&w, "Owner prefers tabs", Category::Preferences);
    let b = seed(&w, "Emma deploy runbook", Category::Workflows);
    let c = seed(&w, "Trey answers first", Category::People);
    w.pin(&b.slug).unwrap();
    w.unpin(&b.slug).unwrap();
    w.update(&b.slug, "# Emma deploy runbook\n\nStep one.", None)
        .unwrap();
    w.pin(&a.slug).unwrap();
    w.archive(&c.slug).unwrap();

    let after_mutations = std::fs::read_to_string(&index).unwrap();
    w.rebuild_index().unwrap();
    let after_rebuild = std::fs::read_to_string(&index).unwrap();
    assert_eq!(
        after_mutations, after_rebuild,
        "index.md drifted from pages/ — a mutation skipped its bookkeeping"
    );
    assert!(after_mutations.contains("owner-prefers-tabs"));
    assert!(
        after_mutations.contains("Owner prefers tabs"),
        "the title is the catalog entry"
    );
    assert!(
        after_mutations.contains("## archive"),
        "an archived page is catalogued, not hidden:\n{after_mutations}"
    );
}

/// The other half of the same guarantee, and the one a rebuild-versus-mutation
/// comparison cannot see: a rebuild reads the directory rather than trusting
/// whatever the index already said.
#[test]
fn a_page_deleted_by_hand_leaves_the_index_on_the_next_rebuild() {
    let (dir, w) = wiki();
    seed(&w, "Alpha", Category::Facts);
    seed(&w, "Beta", Category::Facts);
    std::fs::remove_file(dir.path().join(".emma/memory/pages/beta.md")).unwrap();
    let index = w.rebuild_index().unwrap();
    assert!(index.contains("alpha"), "{index}");
    assert!(!index.contains("beta"), "{index}");
}

#[test]
fn every_mutation_appends_one_parseable_log_line() {
    let (dir, w) = wiki();
    let p = seed(&w, "Alpha", Category::Facts);
    w.pin(&p.slug).unwrap();
    w.unpin(&p.slug).unwrap();
    w.update(&p.slug, "Changed.", None).unwrap();
    w.archive(&p.slug).unwrap();
    let log = std::fs::read_to_string(dir.path().join(".emma/memory/log.md")).unwrap();
    let headers: Vec<&str> = log.lines().filter(|l| l.starts_with("## [")).collect();
    assert_eq!(headers.len(), 5, "one header per mutation:\n{log}");
    for (h, op) in headers
        .iter()
        .zip(["create", "pin", "unpin", "update", "archive"])
    {
        assert!(
            h.contains(&format!("] {op} | alpha")),
            "expected `## [date] {op} | alpha`, got {h}"
        );
    }
    assert!(
        log.find("create").unwrap() < log.find("archive").unwrap(),
        "append-only, oldest first"
    );
}

/// The guarantee: a page the store cannot read is reported with its reason and
/// left where it is. Not skipped, not fatal, not deleted.
#[test]
fn a_malformed_page_is_reported_by_list_and_is_never_fatal_and_never_silent() {
    let (dir, w) = wiki();
    seed(&w, "Good", Category::Facts);
    let bad = dir.path().join(".emma/memory/pages/broken.md");
    std::fs::write(&bad, "---\ncategory: facts\npinned: maybe\n---\n# Broken\n").unwrap();
    let headless = dir.path().join(".emma/memory/pages/headless.md");
    std::fs::write(&headless, "no frontmatter at all\n").unwrap();

    let listing = w
        .list(Filter::default())
        .expect("a broken page is not a fatal error");
    assert_eq!(slugs(&listing), vec!["good"], "the good page still lists");
    let mut names: Vec<&str> = listing.malformed.iter().map(|m| m.slug.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["broken", "headless"]);
    assert!(
        listing.malformed.iter().all(|m| !m.problem.is_empty()),
        "a malformed page must say what is wrong with it"
    );
    assert!(
        listing
            .malformed
            .iter()
            .any(|m| m.problem.contains("maybe")),
        "the reason must quote what it choked on: {:?}",
        listing.malformed
    );

    // The index does not quietly drop them either, and the files are still on
    // disk: reporting a page is not a licence to remove it.
    w.rebuild_index().unwrap();
    let index = std::fs::read_to_string(dir.path().join(".emma/memory/index.md")).unwrap();
    assert!(
        index.contains("broken") && index.contains("headless"),
        "{index}"
    );
    assert!(bad.is_file() && headless.is_file());
    assert_eq!(w.counts().unwrap().malformed, 2);
}

#[test]
fn an_unknown_category_is_malformed_rather_than_accepted() {
    let (dir, w) = wiki();
    std::fs::write(
        dir.path().join(".emma/memory/pages/odd.md"),
        "---\ncategory: vibes\npinned: false\ncreated: 2026-08-26\nsource: x\n---\n# Odd\n",
    )
    .unwrap();
    let listing = w.list(Filter::default()).unwrap();
    assert!(listing.pages.is_empty());
    assert_eq!(listing.malformed.len(), 1);
    assert!(
        listing.malformed[0].problem.contains("vibes"),
        "{:?}",
        listing.malformed[0]
    );
}

#[test]
fn reading_a_slug_that_does_not_exist_says_so_rather_than_panicking() {
    let (_d, w) = wiki();
    let e = w.read("nothing-here").unwrap_err();
    assert!(e.to_string().contains("nothing-here"), "{e}");
}

#[test]
fn the_summary_the_page_reads_is_recent_first_and_carries_the_pinned_and_the_counts() {
    let (_d, w) = wiki();
    let a = seed(&w, "First", Category::Facts);
    let _b = seed(&w, "Second", Category::Projects);
    let c = seed(&w, "Third", Category::People);
    w.pin(&c.slug).unwrap();
    w.archive(&a.slug).unwrap();

    let view = w.view().unwrap();
    assert_eq!(view.counts.total, 2);
    assert_eq!(
        view.pinned
            .iter()
            .map(|e| e.slug.as_str())
            .collect::<Vec<_>>(),
        vec!["third"]
    );
    let recent: Vec<&str> = view.recent.iter().map(|e| e.slug.as_str()).collect();
    assert_eq!(
        recent,
        vec!["third", "second"],
        "most recently touched first"
    );
    assert_eq!(view.recent[0].title, "Third");
    assert_eq!(view.recent[0].category, Category::People);
    assert!(view.malformed.is_empty());
    assert_eq!(view.root, w.root());
}

#[test]
fn a_view_is_a_card_and_stops_at_the_recent_cap() {
    let (_d, w) = wiki();
    for n in 0..RECENT + 3 {
        seed(&w, &format!("Memory {n}"), Category::Facts);
    }
    let view = w.view().unwrap();
    assert_eq!(view.counts.total, RECENT + 3);
    assert_eq!(view.recent.len(), RECENT, "the card is capped");
}

#[test]
fn a_hand_written_page_dropped_into_pages_is_picked_up_by_a_rebuild() {
    let (dir, w) = wiki();
    seed(&w, "Made by the library", Category::Facts);
    std::fs::write(
        dir.path().join(".emma/memory/pages/by-hand.md"),
        "---\ncategory: references\npinned: true\ncreated: 2026-08-20\nsource: the owner\n---\n\n# Written by hand\n\nBody.\n",
    )
    .unwrap();
    w.rebuild_index().unwrap();
    let index = std::fs::read_to_string(dir.path().join(".emma/memory/index.md")).unwrap();
    assert!(index.contains("Written by hand"), "{index}");
    assert_eq!(w.counts().unwrap().pinned, 1);
    assert!(w.read("by-hand").unwrap().front.pinned);
}

// ---------------------------------------------------------------------------
// The frontmatter reader
//
// A parser in this workspace once dropped 86 of 90 real files because its
// fixtures were written on LF and the files it met were CRLF. These are the
// shapes a real editor produces, and the shapes a hand edit produces when it
// goes wrong.
// ---------------------------------------------------------------------------

/// Write a page file byte for byte, bypassing `create`, and read it back
/// through the store. The point is the bytes: a helper that went through
/// `render` would only ever test the writer against itself.
fn on_disk(w: &Wiki, slug: &str, text: &str) -> Result<Page> {
    std::fs::write(w.pages_dir().join(format!("{slug}.md")), text).unwrap();
    w.read(slug)
}

#[test]
fn a_page_written_with_crlf_line_endings_parses() {
    let (_d, w) = wiki();
    let text = "---\r\ncategory: facts\r\npinned: true\r\ncreated: 2026-08-26\r\n\
                source: the owner\r\n---\r\n\r\n# A windows page\r\n\r\nBody.\r\n";
    let p = on_disk(&w, "crlf", text).expect("a CRLF page is a page");
    assert_eq!(p.front.category, Category::Facts);
    assert!(p.front.pinned);
    assert_eq!(
        p.front.created, "2026-08-26",
        "the CR is not part of the value"
    );
    assert_eq!(p.front.source, "the owner");
    assert_eq!(p.title, "A windows page");
    assert!(p.body.starts_with("# A windows page"), "{:?}", p.body);
}

#[test]
fn a_utf8_bom_in_front_of_the_frontmatter_is_not_a_malformed_page() {
    let (_d, w) = wiki();
    let text = "\u{feff}---\ncategory: people\npinned: false\ncreated: 2026-08-26\n\
                source: notepad\n---\n\n# Byte order mark\n";
    let p = on_disk(&w, "bom", text).expect("a BOM is an editor artefact, not a page's content");
    assert_eq!(p.front.category, Category::People);
    assert_eq!(p.title, "Byte order mark");
}

#[test]
fn a_bom_and_crlf_together_still_parse() {
    let (_d, w) = wiki();
    let text = "\u{feff}---\r\ncategory: people\r\ncreated: 2026-08-26\r\n\
                source: notepad\r\n---\r\n\r\n# Both\r\n";
    let p = on_disk(&w, "both", text).expect("the two artefacts arrive together");
    assert_eq!(p.title, "Both");
    assert!(!p.front.pinned, "a missing `pinned:` is not pinned");
}

#[test]
fn every_way_a_frontmatter_can_be_wrong_names_what_is_wrong_with_it() {
    let (_d, w) = wiki();
    let cases: &[(&str, &str, &str)] = &[
        ("none", "# Just a body\n", "no frontmatter"),
        (
            "unclosed",
            "---\ncategory: facts\ncreated: 2026-08-26\nsource: x\n",
            "never closed",
        ),
        (
            "nokey",
            "---\ncategory facts\ncreated: 2026-08-26\nsource: x\n---\n# B\n",
            "key: value",
        ),
        (
            "unknown-key",
            "---\ncategory: facts\ncreated: 2026-08-26\nsource: x\nauthor: me\n---\n# B\n",
            "`author` is not a memory frontmatter key",
        ),
        (
            "twice",
            "---\ncategory: facts\ncategory: people\ncreated: 2026-08-26\nsource: x\n---\n# B\n",
            "appears twice",
        ),
        (
            "no-source",
            "---\ncategory: facts\ncreated: 2026-08-26\n---\n# B\n",
            "no `source:` line",
        ),
        (
            "no-created",
            "---\ncategory: facts\nsource: x\n---\n# B\n",
            "no `created:` line",
        ),
        (
            "no-category",
            "---\ncreated: 2026-08-26\nsource: x\n---\n# B\n",
            "no `category:` line",
        ),
        (
            "bad-bool",
            "---\ncategory: facts\npinned: yes\ncreated: 2026-08-26\nsource: x\n---\n# B\n",
            "not a boolean",
        ),
    ];
    for (slug, text, expected) in cases {
        let e = on_disk(&w, slug, text)
            .expect_err(&format!("`{slug}` should not have parsed:\n{text}"));
        let said = format!("{e:#}");
        assert!(
            said.contains(expected),
            "`{slug}` should have said `{expected}`, said `{said}`"
        );
        assert!(
            said.contains(slug),
            "the message must name the page: {said}"
        );
    }
}

#[test]
fn an_awkward_source_survives_the_round_trip_because_the_writer_quotes_it() {
    let (_d, w) = wiki();
    // Three colons, a `#` that is a fragment and not a comment, and a token
    // YAML would otherwise read as a boolean.
    let awkward = "url: https://example.invalid/a#b: yes";
    let p = w
        .create("Odd source", Category::References, awkward, "Body.")
        .unwrap();
    assert_eq!(w.read(&p.slug).unwrap().front.source, awkward);

    let raw = std::fs::read_to_string(w.pages_dir().join("odd-source.md")).unwrap();
    assert!(
        raw.contains(r#"source: "url: https://example.invalid/a#b: yes""#),
        "the writer must quote it:\n{raw}"
    );
}

#[test]
fn a_source_with_a_windows_path_or_a_quote_in_it_round_trips() {
    let (_d, w) = wiki();
    for (n, source) in [
        r"C:\Users\owner\notes\a.md",
        r#"the owner said "use int""#,
        "  leading and trailing  ",
        "- not a list item",
        "",
    ]
    .into_iter()
    .enumerate()
    {
        let p = w
            .create(&format!("Page {n}"), Category::Facts, source, "Body.")
            .unwrap();
        assert_eq!(
            w.read(&p.slug).unwrap().front.source,
            source,
            "`{source}` did not survive"
        );
    }
}

#[test]
fn a_hand_quoted_value_is_unquoted_once_and_not_twice() {
    let (_d, w) = wiki();
    let p = on_disk(
        &w,
        "quoted",
        "---\ncategory: facts\ncreated: \"2026-08-26\"\nsource: 'it''s mine'\n---\n# Q\n",
    )
    .expect("a hand-quoted page");
    assert_eq!(p.front.created, "2026-08-26");
    assert_eq!(
        p.front.source, "it's mine",
        "single quotes unescape as YAML's do"
    );

    let bare = on_disk(
        &w,
        "bare-quote",
        "---\ncategory: facts\ncreated: 2026-08-26\nsource: \"\n---\n# Q\n",
    )
    .expect("a lone quote is text");
    assert_eq!(bare.front.source, "\"");
}

#[test]
fn a_body_with_a_horizontal_rule_in_it_is_not_read_as_the_end_of_the_frontmatter() {
    let (_d, w) = wiki();
    let p = w
        .create(
            "Ruled",
            Category::Facts,
            "session",
            "# Ruled\n\nOne.\n\n---\n\nTwo.\n",
        )
        .unwrap();
    let read = w.read(&p.slug).unwrap();
    assert!(read.body.contains("One."), "{}", read.body);
    assert!(
        read.body.contains("---") && read.body.contains("Two."),
        "the rule and everything after it belong to the body: {}",
        read.body
    );
}

#[test]
fn a_slug_can_only_ever_name_a_file_inside_the_wiki() {
    // The path guard is `slugify`, so this is the test that a title full of
    // separators cannot climb out of `pages/`.
    for hostile in [
        "../../etc/passwd",
        "..",
        "C:\\Windows\\System32",
        "a/b/c",
        "  ",
        "…",
        "con",
    ] {
        let slug = slugify(hostile);
        assert!(
            !slug.is_empty()
                && slug
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "`{hostile}` produced `{slug}`"
        );
        assert!(!slug.starts_with('-') && !slug.ends_with('-'), "{slug}");
    }
    assert_eq!(slugify("../../etc/passwd"), "etc-passwd");
    assert_eq!(
        slugify("…"),
        "memory",
        "a title with nothing left still names a file"
    );
    assert!(
        slugify(&"x".repeat(200)).len() <= 60,
        "a slug is a filename"
    );
}

#[test]
fn a_title_that_is_all_separators_still_creates_a_page_and_numbers_the_next_one() {
    let (_d, w) = wiki();
    let a = w.create("…", Category::Facts, "s", "One.").unwrap();
    let b = w.create("!!!", Category::Facts, "s", "Two.").unwrap();
    assert_eq!(a.slug, "memory");
    assert_eq!(b.slug, "memory-2");
}

#[test]
fn a_category_parses_from_text_and_refuses_anything_outside_the_closed_set() {
    assert_eq!(Category::parse("Facts").unwrap(), Category::Facts);
    assert_eq!(Category::parse("  people  ").unwrap(), Category::People);
    let e = Category::parse("vibes").unwrap_err();
    assert!(e.to_string().contains("vibes"), "{e}");
    for c in Category::ALL {
        assert!(
            e.to_string().contains(c.as_str()),
            "the error must name the whole set: {e}"
        );
        assert_eq!(Category::parse(c.as_str()).unwrap(), c);
    }
}
