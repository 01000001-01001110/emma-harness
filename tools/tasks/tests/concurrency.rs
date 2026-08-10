//! What happens when the person and the agent both have the file.
//!
//! There is no lock to take, so the guarantee on offer is narrow and worth
//! stating exactly: a change that lands on disk *after* the agent read the file
//! is never overwritten from the agent's stale copy. The agent re-reads and
//! re-applies its own change on top of what it found.
//!
//! What is *not* on offer is in `store.rs`: an editor that writes its whole
//! in-memory buffer wins, because from the filesystem's side that is simply a
//! newer complete file.

mod support;

use emma_tools_tasks::store::{self, Collision};
use serde_json::json;
use support::Project;

/// The guard itself, at the level it is implemented. `replace_if_unchanged` is
/// public precisely so a test can hand it a stamp that has gone stale — the
/// retry loop above it never surfaces one, so through the tool surface this
/// failure is invisible until the day it eats somebody's line.
#[tokio::test]
async fn a_write_that_arrived_after_the_read_is_not_clobbered() {
    let project = Project::new();
    let file = project.tasks_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();

    // The agent reads.
    std::fs::write(&file, "- [ ] a `#0001`\n").unwrap();
    let (_doc, stamp) = store::load(&file).unwrap();

    // The person saves while the agent is deciding what to write.
    std::fs::write(&file, "- [ ] a `#0001`\n- [ ] their new task\n").unwrap();

    // The agent's write, from its stale picture, must not land.
    let result = store::replace_if_unchanged(&file, stamp, "- [x] a `#0001`\n");
    assert!(
        matches!(result, Err(Collision)),
        "a stale write was allowed through"
    );
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("their new task"),
        "the human's edit was overwritten"
    );
}

/// Detecting the collision is only half the job; a `Collision` that surfaced as
/// an error would make every concurrent save a failed tool call. The assertion
/// on `attempts` is what stops this passing vacuously — without it, an
/// implementation that never collided at all would look identical.
#[tokio::test]
async fn the_retry_re_applies_the_change_to_what_it_found() {
    let project = Project::new();
    let file = project.tasks_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "- [ ] a `#0001`\n").unwrap();

    // `edit` re-runs its closure against a freshly read document. Simulate the
    // race by mutating the file from inside the first attempt, exactly as a
    // person saving mid-call would.
    let mut attempts = 0;
    let outcome = store::edit(&file, |doc| {
        attempts += 1;
        if attempts == 1 {
            std::fs::write(&file, "- [ ] a `#0001`\n- [ ] theirs `#0002`\n").unwrap();
        }
        assert!(doc.update("0001", Some(emma_tools_tasks::Status::Completed), None));
        Ok(())
    });

    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(attempts >= 2, "the collision was not detected at all");
    let after = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        after, "- [x] a `#0001`\n- [ ] theirs `#0002`\n",
        "the retry lost one of the two changes"
    );
}

#[tokio::test]
async fn a_concurrent_edit_to_another_task_survives_an_update() {
    // The same property through the public surface, which is where it matters.
    let project = Project::new();
    project.hand_write("- [ ] a `#0001`\n- [ ] b `#0002`\n");
    project
        .ok(
            "TaskUpdate",
            json!({ "id": "0002", "status": "in_progress" }),
        )
        .await;
    assert_eq!(project.read_tasks(), "- [ ] a `#0001`\n- [~] b `#0002`\n");
}

#[tokio::test]
async fn a_partly_written_file_is_never_visible() {
    // The replace is a rename, so a reader sees the old file or the new one.
    // The temp file must not be left behind either — a stray tasks.md.tmp in
    // someone's repository is a bug report.
    let project = Project::new();
    project.ok("TaskCreate", json!({ "tasks": ["a"] })).await;
    let dir = project.tasks_file().parent().unwrap().to_path_buf();
    let strays: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "tasks.md")
        .collect();
    assert!(strays.is_empty(), "left behind: {strays:?}");
}
