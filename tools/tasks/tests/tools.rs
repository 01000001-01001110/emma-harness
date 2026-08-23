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

/// Two steps of a plan can genuinely read the same — "run the tests" appears
/// twice in half the plans anyone writes — and `TaskCreate` is documented as
/// not deduplicating them. What it must not do is hand back one handle for
/// both, or two lines carrying the same one: `TaskGet` cannot see a duplicate
/// and would answer confidently about whichever line it reached first, so the
/// model would tick off a step it never did. The second call is here because
/// the salting has to survive a re-read of the file as well as a single batch.
#[tokio::test]
async fn identical_task_text_still_gets_distinct_handles() {
    let project = Project::new();
    let first = project
        .ok(
            "TaskCreate",
            json!({ "tasks": ["run the tests", "run the tests"] }),
        )
        .await;
    let second = project
        .ok("TaskCreate", json!({ "tasks": ["run the tests"] }))
        .await;

    let mut handles = ids(&first);
    handles.extend(ids(&second));
    assert_eq!(handles.len(), 3, "{first:?} {second:?}");
    let mut unique = handles.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        3,
        "two tasks share a handle, so TaskGet is ambiguous: {handles:?}"
    );

    // And each of them addresses a line that is really there.
    for id in &handles {
        project.ok("TaskGet", json!({ "id": id })).await;
    }
    let file = project.read_tasks();
    assert_eq!(
        file.matches("- [ ] run the tests").count(),
        3,
        "a task was deduplicated away:\n{file}"
    );
}

/// The batch is the unit. A plan whose fourth entry is malformed must write
/// none of the first three, or the model is left holding a half-written list
/// it has no ids for and no way to tell which half landed. The existing
/// refusal test starts from no file at all, so it cannot see the failure this
/// one catches: good entries being written *around* the bad one into a file
/// that already exists.
#[tokio::test]
async fn one_bad_entry_in_a_batch_writes_none_of_it() {
    let project = Project::new();
    project.hand_write("- [ ] already here `#0001`\n");
    let before = fingerprint(project.root());

    let error = project
        .err(
            "TaskCreate",
            json!({ "tasks": ["good one", "good two", { "text": "   " }] }),
        )
        .await;
    assert_eq!(error.kind(), "bad_arguments");
    assert_eq!(
        before,
        fingerprint(project.root()),
        "a batch with a bad entry wrote its good entries anyway"
    );

    // The negative control: the same batch minus the bad entry does write, so
    // the assertion above is about the rejection and not about a tool that
    // writes nothing under any circumstances.
    project
        .ok("TaskCreate", json!({ "tasks": ["good one", "good two"] }))
        .await;
    let after = project.read_tasks();
    assert!(after.contains("- [ ] good one"), "{after}");
    assert!(after.contains("- [ ] good two"), "{after}");
}

/// A model that has used another harness will send `{"text": ..., "id": ...}`
/// or `{"task": ...}` sooner or later. Ignoring the extra key silently is the
/// expensive answer: an `id` the model believes it chose, and does not get,
/// becomes a `TaskUpdate` against a handle that was never written. Naming the
/// key that was not understood is one turn; a wrong handle is a whole branch
/// of confused calls.
#[tokio::test]
async fn a_task_object_with_a_key_that_is_not_a_key_is_refused() {
    let project = Project::new();
    let error = project
        .err(
            "TaskCreate",
            json!({ "tasks": [{ "text": "port it", "id": "a1b2" }] }),
        )
        .await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("id"), "{error}");
    assert!(
        error.detail().contains("text"),
        "the message has to say what is accepted: {error}"
    );
    assert!(
        !project.tasks_file().exists(),
        "a refused call created the file anyway"
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

/// Document order is information — the sequence a person put their tasks in is
/// the order they mean them to be done — and `render::block` is documented as
/// never sorting. Nothing else in the suite would notice a sort: every other
/// fixture is written in an order that happens to agree with alphabetical or
/// with handle order, so a `sort_by_key` slipped into `TaskList` would leave
/// the whole file green while telling the model to start with the wrong step.
#[tokio::test]
async fn list_returns_tasks_in_the_files_own_order() {
    let project = Project::new();
    // Deliberately the reverse of both alphabetical order and handle order, so
    // neither sort can pass by coincidence.
    project.hand_write("- [ ] zulu `#00ff`\n- [ ] mike `#00aa`\n- [ ] alpha `#0001`\n");

    let listed = project.ok("TaskList", json!({})).await;
    let order: Vec<usize> = ["zulu", "mike", "alpha"]
        .iter()
        .map(|w| {
            listed
                .content
                .find(w)
                .unwrap_or_else(|| panic!("{w} missing: {listed:?}"))
        })
        .collect();
    assert!(
        order[0] < order[1] && order[1] < order[2],
        "the list was reordered; the plan's sequence is not the tool's to choose: {listed:?}"
    );
}

/// The claim `TaskList` makes in its own module doc, end to end: it hands the
/// model an id for a task the file has never been written to name, and asking
/// for that id back still costs no write. It is what lets the loop read a
/// hand-written list — nobody types `` `#a1b2` `` after their own tasks — and
/// address it without first rewriting the person's file underneath them.
/// The fingerprint spans both calls, so a reader that stamped handles to make
/// the lookup work would be caught even though the lookup itself succeeded.
#[tokio::test]
async fn a_task_the_file_never_named_is_still_addressable_without_a_write() {
    let project = Project::new();
    project
        .hand_write("- [ ] port the middleware\n  the trait is in src/auth.rs\n- [~] wire it up\n");
    let before = fingerprint(project.root());

    let listed = project.ok("TaskList", json!({})).await;
    let handles = ids(&listed);
    assert_eq!(handles.len(), 2, "{listed:?}");

    let got = project.ok("TaskGet", json!({ "id": &handles[0] })).await;
    assert!(got.content.contains("port the middleware"), "{got:?}");
    assert!(
        got.content.contains("the trait is in src/auth.rs"),
        "the note under an unstamped task is still the human's: {got:?}"
    );

    assert_eq!(
        before,
        fingerprint(project.root()),
        "reading a hand-written list rewrote it"
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

/// Status and text arrive together whenever a model starts a step and sharpens
/// the wording in the same breath, and that call takes a branch neither
/// single-argument test reaches: the line is re-emitted from its fields
/// *including* the glyph, rather than spliced. A glyph read from the wrong
/// place there loses the status change while reporting success — the task
/// reads as still pending and gets done twice. The second half is the negative
/// control: a status-only call must leave the words exactly as they were, so
/// this is not passing on a tool that rewrites the line every time.
#[tokio::test]
async fn update_can_change_the_status_and_the_words_in_one_call() {
    let project = Project::new();
    project.hand_write("- [ ] first `#0001`\n- [ ] second `#0002`\n");

    project
        .ok(
            "TaskUpdate",
            json!({ "id": "0001", "status": "in_progress", "text": "port the auth middleware" }),
        )
        .await;
    assert_eq!(
        project.read_tasks(),
        "- [~] port the auth middleware `#0001`\n- [ ] second `#0002`\n",
        "one of the two changes was dropped, or the neighbour was rewritten"
    );

    project
        .ok("TaskUpdate", json!({ "id": "0002", "status": "completed" }))
        .await;
    assert_eq!(
        project.read_tasks(),
        "- [~] port the auth middleware `#0001`\n- [x] second `#0002`\n",
        "a status-only call changed the words"
    );
}

/// The id `TaskList` derives from a task's text is the id the model then
/// passes to `TaskUpdate` — and that update is the first write, so it is where
/// the handle gets stamped into the file. If the stamped handle were not the
/// derived one the model was told, its next call would name a task that no
/// longer exists, on the very turn it did the work. The `TaskGet` afterwards
/// is the point: the id has to keep working across the write that changed the
/// file from unnamed to named.
#[tokio::test]
async fn an_update_stamps_the_same_handle_the_reader_handed_out() {
    let project = Project::new();
    project.hand_write("- [ ] port the middleware\n");
    assert!(
        !project.read_tasks().contains('#'),
        "the fixture must start with no handle in it"
    );

    let id = ids(&project.ok("TaskList", json!({})).await).remove(0);
    project
        .ok("TaskUpdate", json!({ "id": &id, "status": "completed" }))
        .await;

    assert_eq!(
        project.read_tasks(),
        format!("- [x] port the middleware `#{id}`\n"),
        "the write stamped a handle the model was never given"
    );
    let got = project.ok("TaskGet", json!({ "id": &id })).await;
    assert!(got.content.contains("port the middleware"), "{got:?}");
}

// endregion: Ticking a box, and the round trip
