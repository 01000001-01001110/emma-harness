//! The tests that decide whether anyone ever trusts this file.
//!
//! Every fixture here is written the way a *person* would write it, not the
//! way the tool would: headings the writer never emits, prose between tasks, a
//! task numbered `1.`, a nested sub-item, a box ticked by hand, tasks in an
//! order nothing sorted. The agent then works on the file, and everything the
//! person put there has to still be there afterwards.
//!
//! Losing one of those lines is not a cosmetic bug. It is the moment the person
//! stops trusting the file, and after that they stop reading it, and after that
//! the whole feature is decoration.

mod support;

use serde_json::json;
use support::Project;

/// Deliberately unlike anything the writer produces: no preamble, `*` and `1.`
/// bullets, a heading in the middle, prose at both ends, a hand-ticked box, and
/// two tasks with no handle at all.
const BY_HAND: &str = "\
# What I actually need done

Rough order, but do the middleware first — the rest depends on it.

## This week

* [~] port the auth middleware `#a1b2`
  the old SessionStore trait is in src/auth/store.rs
  ask me before changing the public signature
* [ ] make the integration tests pass
1. [x] read the existing middleware

## Someday

- [ ] delete the compat shim
  low priority, do not let this block anything

Anything below here is my own notes, leave it alone.

| what | when |
| ---- | ---- |
| demo | Friday |
";

// region: The agent writes, the human's file survives
// ---------------------------------------------------------------------------
// The agent writes, the human's file survives
//
// One fixture written the way a person writes, three calls the agent makes.
// Everything the person put there has to still be there afterwards — including
// the shapes nobody thought to name.
// ---------------------------------------------------------------------------

/// The load-bearing one. It asserts over *every* line of the fixture rather
/// than a chosen few, so a writer that drops a shape nobody thought to name —
/// the table, the numbered bullet, the blank line inside a note block — fails
/// here without anyone having predicted which shape it would be.
#[tokio::test]
async fn every_hand_written_line_survives_an_update() {
    let project = Project::new();
    project.hand_write(BY_HAND);

    project
        .ok("TaskUpdate", json!({ "id": "a1b2", "status": "completed" }))
        .await;

    let after = project.read_tasks();
    for line in BY_HAND.lines() {
        if line.contains("port the auth middleware") {
            continue; // the one line the call was about
        }
        assert!(
            after.contains(line),
            "the update dropped a line the human wrote:\n  {line:?}\n\nfile now:\n{after}"
        );
    }
    assert!(after.contains("* [x] port the auth middleware `#a1b2`"));
}

#[tokio::test]
async fn creating_a_task_keeps_the_notes_the_order_and_the_trailing_prose() {
    let project = Project::new();
    project.hand_write(BY_HAND);

    project
        .ok("TaskCreate", json!({ "tasks": ["write the changelog"] }))
        .await;

    let after = project.read_tasks();
    for line in BY_HAND.lines() {
        if line.starts_with("* [ ] make") || line.starts_with("1. [x]") || line.starts_with("- [ ]")
        {
            continue; // gains a handle; checked separately below
        }
        assert!(after.contains(line), "dropped:\n  {line:?}\n\n{after}");
    }

    // A new task joins the list rather than landing after the human's table.
    let new_at = after.find("write the changelog").expect("new task missing");
    let table_at = after.find("| demo | Friday |").expect("table missing");
    assert!(
        new_at < table_at,
        "the new task landed below the human's own notes:\n{after}"
    );

    // The section headings are still where they were, in the same order.
    assert!(after.find("## This week") < after.find("## Someday"));
}

/// Stamping is the only time a write touches lines the call did not name, so
/// it is the one place a bulk rewrite could quietly restyle the file. Hence the
/// bullets: the `1.` and `*` lines are asserted with their original markers
/// still attached, and the negative assertion catches a stamp that re-rendered
/// the human's `*` as the writer's `-`.
#[tokio::test]
async fn a_task_with_no_handle_gains_one_without_losing_its_words() {
    let project = Project::new();
    project.hand_write(BY_HAND);

    project
        .ok("TaskCreate", json!({ "tasks": ["anything"] }))
        .await;

    let after = project.read_tasks();
    // Append-only: the words are untouched, the handle goes after them.
    assert!(
        after.contains("* [ ] make the integration tests pass `#"),
        "{after}"
    );
    assert!(
        after.contains("1. [x] read the existing middleware `#"),
        "{after}"
    );
    // And the bullet style the human chose is still theirs.
    assert!(
        !after.contains("- [ ] make the integration tests pass"),
        "{after}"
    );
}

// endregion: The agent writes, the human's file survives

// region: The human edits, the agent copes
// ---------------------------------------------------------------------------
// The human edits, the agent copes
//
// The other direction, and the one with no lock behind it. A person ticks a
// box, prunes a done line, or starts from a file with no tasks in it at all.
// None of those may read to the agent as machinery failing.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_hand_ticked_box_is_read_as_completed() {
    // The single most likely thing a human does to this file.
    let project = Project::new();
    project.hand_write("- [ ] a `#0001`\n- [ ] b `#0002`\n");

    // They tick the first one while the agent is thinking.
    project.hand_write("- [x] a `#0001`\n- [ ] b `#0002`\n");

    let open = project.ok("TaskList", json!({})).await;
    assert!(!open.content.contains("#0001"), "{open:?}");
    assert!(open.content.contains("1 completed"), "{open:?}");
}

#[tokio::test]
async fn a_task_deleted_by_hand_is_simply_gone() {
    // Pruning the done lines is the intended way the file stays short. The
    // agent must cope with its own task having vanished rather than treat it
    // as machinery failing.
    let project = Project::new();
    project.hand_write("- [ ] a `#0001`\n- [ ] b `#0002`\n");
    project.hand_write("- [ ] b `#0002`\n");

    let error = project
        .err("TaskUpdate", json!({ "id": "0001", "status": "completed" }))
        .await;
    assert_eq!(error.kind(), "bad_arguments");
    let list = project.ok("TaskList", json!({})).await;
    assert!(list.content.contains("#0002"), "{list:?}");
}

/// A file with no tasks in it takes the branch that has no last task to append
/// after, and it must not be mistaken for an empty file and replaced by the
/// preamble. The trailing-newline assertion guards the off-by-one in
/// `insertion_point`: appending past the file's final empty line leaves a file
/// that does not end in a newline, and every later append compounds it.
#[tokio::test]
async fn a_file_of_pure_prose_gains_a_task_and_keeps_the_prose() {
    let project = Project::new();
    project.hand_write("# Notes\n\nnothing here is a task.\n");

    project
        .ok("TaskCreate", json!({ "tasks": ["the first task"] }))
        .await;

    let after = project.read_tasks();
    assert!(after.contains("nothing here is a task."), "{after}");
    assert!(after.contains("- [ ] the first task `#"), "{after}");
    assert!(after.ends_with('\n'), "{after:?}");
}

// endregion: The human edits, the agent copes
