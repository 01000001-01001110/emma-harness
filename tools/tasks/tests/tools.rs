//! The four tools, and the edges where each could have done something
//! plausible and wrong instead.
//!
//! `round_trip.rs` covers the human's file surviving; `concurrency.rs` covers
//! two writers; `containment.rs` covers where the file may live. This file
//! covers the contract each tool advertises to the model: which argument
//! shapes are accepted, which failures are `BadArguments` rather than empty
//! results, and — the two that would be silent — that the readers write
//! nothing and that an id handed out by one tool is accepted by the others.

mod support;

use serde_json::json;
use support::{fingerprint, ids, Project};

// region: Writing a plan down
// ---------------------------------------------------------------------------
// Writing a plan down
//
// `TaskCreate` takes the whole plan in one call, so the edges are the argument
// shapes it accepts and the guarantee that a list with one bad entry writes
// none of it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_makes_the_file_and_returns_ids() {
    let project = Project::new();
    assert!(!project.tasks_file().exists());

    let outcome = project
        .ok(
            "TaskCreate",
            json!({ "tasks": ["read the middleware", "port it", "make the tests pass"] }),
        )
        .await;

    let handles = ids(&outcome);
    assert_eq!(handles.len(), 3, "{outcome:?}");
    let file = project.read_tasks();
    assert!(file.contains("- [ ] read the middleware"), "{file}");
    assert!(
        file.ends_with('\n'),
        "the file must end with a newline:\n{file}"
    );
    // The preamble is addressed to a human and is the reason the file is
    // markdown at all. A file that arrives without it is a file nobody knows
    // they are allowed to edit.
    assert!(file.starts_with("# Tasks\n"), "{file}");
}

/// The two accepted shapes are one branch and one test here against a required
/// wrapper the model would get wrong occasionally forever. Delete this and the
/// object form can rot without anything going red, because the string form is
/// what every other test in this file uses.
#[tokio::test]
async fn create_accepts_objects_with_a_status() {
    let project = Project::new();
    project
        .ok(
            "TaskCreate",
            json!({ "tasks": [{ "text": "port it", "status": "in_progress" }] }),
        )
        .await;
    assert!(project.read_tasks().contains("- [~] port it"));
}

/// `blocked` is the status a model reaches for and this format does not have,
/// so it is the realistic bad argument rather than a made-up one. The second
/// assertion is the one that matters: validation happens before anything
/// touches the disk, so a rejected call leaves no half-written file behind.
#[tokio::test]
async fn create_refuses_a_status_that_is_not_one() {
    let project = Project::new();
    let error = project
        .err(
            "TaskCreate",
            json!({ "tasks": [{ "text": "x", "status": "blocked" }] }),
        )
        .await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("in_progress"), "{error}");
    assert!(
        !project.tasks_file().exists(),
        "a refused call created the file anyway"
    );
}

/// Catches the failure a stateless writer makes: rendering the document it was
/// handed instead of the one on disk, so the second call replaces the first
/// call's work. Order is asserted as well as presence, because a list that
/// arrives in a different order than it was planned in is a different plan.
#[tokio::test]
async fn create_appends_rather_than_replacing() {
    let project = Project::new();
    project
        .ok("TaskCreate", json!({ "tasks": ["first"] }))
        .await;
    project
        .ok("TaskCreate", json!({ "tasks": ["second"] }))
        .await;
    let file = project.read_tasks();
    assert!(file.contains("first") && file.contains("second"), "{file}");
    assert!(
        file.find("first") < file.find("second"),
        "order was not preserved:\n{file}"
    );
}

// endregion: Writing a plan down

// region: Naming one task
// ---------------------------------------------------------------------------
// Naming one task
//
// `TaskGet` is where the human's notes reach the model, and where an id that is
// not in the file has to read as a bad argument rather than as an empty result
// — otherwise the model's repair is to create the task again.
// ---------------------------------------------------------------------------

/// Notes are the whole reason `TaskGet` exists as a separate call from
/// `TaskList`. Without this, the notes could quietly stop being collected and
/// every other test would still pass — `TaskList` deliberately never shows
/// them, so nothing else in the suite would notice.
#[tokio::test]
async fn get_returns_the_task_and_its_notes() {
    let project = Project::new();
    project.hand_write("- [ ] port the middleware `#a1b2`\n  the trait is in src/auth.rs\n");

    let outcome = project.ok("TaskGet", json!({ "id": "a1b2" })).await;
    assert!(
        outcome.content.contains("port the middleware"),
        "{outcome:?}"
    );
    assert!(
        outcome.content.contains("the trait is in src/auth.rs"),
        "the human's note is the reason TaskGet exists: {outcome:?}"
    );
}

/// The file shows `` `#a1b2` `` and `TaskList` prints `#a1b2`, so a model will
/// pass the hash back roughly half the time. Rejecting one of the two forms
/// would be a lookup failure that looks exactly like a deleted task, and the
/// model's repair for a deleted task is to create it again.
#[tokio::test]
async fn get_accepts_the_handle_with_or_without_its_hash() {
    let project = Project::new();
    project.hand_write("- [x] done `#a1b2`\n");
    project.ok("TaskGet", json!({ "id": "#a1b2" })).await;
    project.ok("TaskGet", json!({ "id": "a1b2" })).await;
}

#[tokio::test]
async fn get_for_an_unknown_id_is_an_argument_error() {
    // The contract's line: an error is a fact about the call. The call named
    // something that is not there.
    let project = Project::new();
    project.hand_write("- [ ] something `#a1b2`\n");

    let error = project.err("TaskGet", json!({ "id": "ffff" })).await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("ffff"), "{error}");
}

/// The pairing that keeps the two error classes apart. A missing file is not
/// the machinery being absent — `Unavailable` would tell the model to stop
/// using the tool — and it is not `Ok` either, because an id was named and is
/// not there. The count in the message is what tells it which of the two it is
/// actually looking at.
#[tokio::test]
async fn get_against_a_missing_file_is_an_unknown_id_not_a_broken_tool() {
    let project = Project::new();
    let error = project.err("TaskGet", json!({ "id": "ffff" })).await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(
        error.detail().contains("0 tasks"),
        "the model needs to know the list is empty, not that the tool broke: {error}"
    );
}

// endregion: Naming one task

// region: What the loop reads every turn
// ---------------------------------------------------------------------------
// What the loop reads every turn
//
// `TaskList` lands in the context window over and over, so two things are
// defended here: the open-by-default filter that keeps settled work out of it,
// and `read_only` being a fact rather than a declaration.
// ---------------------------------------------------------------------------

/// Two failures at once. Emptiness is a result, so this must be `Ok` — and the
/// obvious implementation of "make sure the file exists first" would create it,
/// which would make `read_only: true` a lie on the tool the loop calls most.
#[tokio::test]
async fn list_on_a_missing_file_is_ok_and_empty() {
    let project = Project::new();
    let outcome = project.ok("TaskList", json!({})).await;
    assert!(outcome.content.contains("no tasks"), "{outcome:?}");
    assert!(
        !project.tasks_file().exists(),
        "listing created the file; TaskList is read_only"
    );
}

/// The default is the whole reason completed tasks can be kept forever. If it
/// flipped to `all`, nothing would break and no other test would fail — the
/// file would simply grow into the context window, one settled task per turn,
/// until a long run stopped fitting.
#[tokio::test]
async fn list_defaults_to_open_and_all_says_otherwise() {
    let project = Project::new();
    project.hand_write("- [ ] a `#0001`\n- [~] b `#0002`\n- [x] c `#0003`\n");

    let open = project.ok("TaskList", json!({})).await;
    assert!(open.content.contains("#0001") && open.content.contains("#0002"));
    assert!(
        !open.content.contains("#0003"),
        "completed tasks are context the loop pays for every turn: {open:?}"
    );
    // The counts still describe the whole file, so "nothing open" never reads
    // as "nothing exists".
    assert!(open.content.contains("1 completed"), "{open:?}");

    let all = project.ok("TaskList", json!({ "status": "all" })).await;
    assert!(all.content.contains("#0003"), "{all:?}");
}

/// Guards the last arm of `keep`, which routes every filter that is not `open`
/// or `all` through the wire spellings. A typo there matches nothing and looks
/// exactly like a project with no in-progress work.
#[tokio::test]
async fn list_filters_by_a_single_status() {
    let project = Project::new();
    project.hand_write("- [ ] a `#0001`\n- [~] b `#0002`\n- [x] c `#0003`\n");

    let active = project
        .ok("TaskList", json!({ "status": "in_progress" }))
        .await;
    assert!(active.content.contains("#0002"));
    assert!(!active.content.contains("#0001"), "{active:?}");
}

/// A filter matching nothing is the world's answer to the question asked, not
/// a failed call — the same line `Grep` draws when it finds no lines. Turning
/// it into an error would also reach the loop's memo of failed calls, which
/// refuses an identical call until something else succeeds: a perfectly good
/// "none" would make the same question unaskable.
#[tokio::test]
async fn list_with_nothing_matching_is_ok_not_an_error() {
    let project = Project::new();
    project.hand_write("- [x] c `#0003`\n");
    let outcome = project.ok("TaskList", json!({ "status": "pending" })).await;
    assert!(outcome.content.contains("no pending"), "{outcome:?}");
}

/// The other half of the previous test. Without validation an unknown filter
/// would fall through `keep` and return an empty list, telling the model there
/// is no work rather than that it asked the wrong question.
#[tokio::test]
async fn list_refuses_a_filter_that_is_not_a_status() {
    let project = Project::new();
    let error = project
        .err("TaskList", json!({ "status": "blocked" }))
        .await;
    assert_eq!(error.kind(), "bad_arguments");
}

#[tokio::test]
async fn the_readers_touch_nothing() {
    // `read_only` is what the approval gate consults, so it has to be a fact
    // rather than a declaration.
    let project = Project::new();
    project.hand_write("## Notes\n\n- [ ] a\n- [x] b `#0002`\n  a note\n");
    let before = fingerprint(project.root());

    for args in [json!({}), json!({ "status": "all" })] {
        let _ = project.call("TaskList", args).await;
    }
    for id in ["0002", "nope"] {
        let _ = project.call("TaskGet", json!({ "id": id })).await;
    }

    assert_eq!(
        before,
        fingerprint(project.root()),
        "a tool declaring read_only modified the tree"
    );
}

// endregion: What the loop reads every turn

// region: Ticking a box, and the round trip
// ---------------------------------------------------------------------------
// Ticking a box, and the round trip
//
// Completion happens in place, rewording keeps the handle, and a failed update
// writes nothing at all. The last two cross the whole surface: an id one tool
// hands out must be one the others accept, and `open_count` is the only thing
// here the model never touches.
// ---------------------------------------------------------------------------

/// The format decision, asserted as an exact file rather than a `contains`.
/// Every rejected alternative — move to a `## Done` section, delete the line,
/// re-render the document from parsed tasks — passes a `contains` check and
/// fails this one. The neighbour is in the fixture for the same reason: a
/// writer that re-emits every line would leave it byte-identical only by luck.
#[tokio::test]
async fn update_marks_completion_in_place() {
    let project = Project::new();
    project.hand_write("- [ ] first `#0001`\n- [ ] second `#0002`\n");

    project
        .ok("TaskUpdate", json!({ "id": "0001", "status": "completed" }))
        .await;

    assert_eq!(
        project.read_tasks(),
        "- [x] first `#0001`\n- [ ] second `#0002`\n",
        "completing a task moved it or rewrote its neighbour"
    );
}

/// Rewording is the operation that would most plausibly drop the handle, since
/// the new text arrives without one. If it did, the task would be
/// unaddressable by the id the model is holding and would acquire a fresh
/// derived one on the next read — the same task, twice, under two names.
/// The status is `[~]` in the fixture to catch a rewrite that resets it.
#[tokio::test]
async fn update_can_reword_without_losing_the_handle() {
    let project = Project::new();
    project.hand_write("- [~] vague `#0001`\n");
    project
        .ok(
            "TaskUpdate",
            json!({ "id": "0001", "text": "port the auth middleware" }),
        )
        .await;
    assert_eq!(
        project.read_tasks(),
        "- [~] port the auth middleware `#0001`\n"
    );
}

/// The failure this catches is subtle: `store::edit` stamps ids and renders
/// before it writes, so an implementation that returned the error *after* the
/// write would still report failure while having silently stamped every
/// unstamped task in the file. The fingerprint is of the whole tree, so a
/// rewrite producing identical-length content still fails it.
#[tokio::test]
async fn update_for_an_unknown_id_is_an_argument_error_and_writes_nothing() {
    let project = Project::new();
    project.hand_write("- [ ] first `#0001`\n");
    let before = fingerprint(project.root());

    let error = project
        .err("TaskUpdate", json!({ "id": "ffff", "status": "completed" }))
        .await;
    assert_eq!(error.kind(), "bad_arguments");
    assert_eq!(
        before,
        fingerprint(project.root()),
        "a failed update wrote the file anyway"
    );
}

/// A tool that reports success for a no-op is lying about what it did, and the
/// model reads that as "the status is now what I meant" when it never said what
/// it meant. Refusing costs one turn and names the missing argument.
#[tokio::test]
async fn update_that_would_change_nothing_is_refused() {
    let project = Project::new();
    project.hand_write("- [ ] first `#0001`\n");
    let error = project.err("TaskUpdate", json!({ "id": "0001" })).await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("change nothing"), "{error}");
}

#[tokio::test]
async fn a_task_created_can_be_got_listed_and_updated_by_the_id_it_returned() {
    // The whole round trip through the public surface, because an id that is
    // returned but not accepted is the failure that would make all of this
    // useless while every individual test still passed.
    let project = Project::new();
    let created = project
        .ok("TaskCreate", json!({ "tasks": ["port the middleware"] }))
        .await;
    let id = ids(&created).remove(0);

    project.ok("TaskGet", json!({ "id": &id })).await;
    project
        .ok("TaskUpdate", json!({ "id": &id, "status": "completed" }))
        .await;

    let open = project.ok("TaskList", json!({})).await;
    assert!(!open.content.contains(&id), "{open:?}");
    let all = project.ok("TaskList", json!({ "status": "all" })).await;
    assert!(all.content.contains(&format!("[x] #{id}")), "{all:?}");
}

/// `open_count` is not reachable through the tool surface, so nothing else in
/// the suite exercises it — and it is what `goal.rs` consults to decide whether
/// to keep asking. Note what the first assertion establishes: a project with no
/// file has zero open tasks and no error, so a loop that has not created a list
/// yet is indistinguishable from one that finished it. That is exactly why the
/// function's own doc says it must never be the sole done-check.
#[tokio::test]
async fn open_count_answers_the_loops_question_without_a_tool_call() {
    let project = Project::new();
    assert_eq!(emma_tools_tasks::open_count(project.root()).unwrap(), 0);

    project
        .ok("TaskCreate", json!({ "tasks": ["a", "b"] }))
        .await;
    assert_eq!(emma_tools_tasks::open_count(project.root()).unwrap(), 2);

    let id = ids(&project.ok("TaskList", json!({})).await).remove(0);
    project
        .ok("TaskUpdate", json!({ "id": id, "status": "completed" }))
        .await;
    assert_eq!(emma_tools_tasks::open_count(project.root()).unwrap(), 1);
}

// endregion: Ticking a box, and the round trip
