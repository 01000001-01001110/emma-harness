//! The four tools, and the edges where each could have done something
//! plausible and wrong instead.

mod support;

use serde_json::json;
use support::{fingerprint, ids, Project};

// ------------------------------------------------------------- create

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

// ---------------------------------------------------------------- get

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

// --------------------------------------------------------------- list

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

#[tokio::test]
async fn list_with_nothing_matching_is_ok_not_an_error() {
    let project = Project::new();
    project.hand_write("- [x] c `#0003`\n");
    let outcome = project.ok("TaskList", json!({ "status": "pending" })).await;
    assert!(outcome.content.contains("no pending"), "{outcome:?}");
}

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

// ------------------------------------------------------------- update

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
