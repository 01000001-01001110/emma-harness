//! How a task reads back to the model.
//!
//! `TaskList` is the call the loop makes most, and its output lands in the
//! context window again on every turn it is made. So the line is the checkbox
//! the human sees, the handle, and the text — nothing else. Notes are left to
//! `TaskGet`: they are the part a person writes at length, and paying for them
//! on every turn is how a task list becomes the reason a session runs out of
//! room.

use crate::doc::{Status, TaskView};

/// One task, one line: `[~] #a3f1  port the auth middleware`.
pub fn line(task: &TaskView) -> String {
    format!(
        "[{}] #{}  {}",
        task.status.glyph(),
        task.id,
        if task.text.is_empty() {
            "(no description)"
        } else {
            &task.text
        }
    )
}

/// The counts, first, so a model that reads one line of the result already
/// knows whether anything is outstanding.
pub fn counts(tasks: &[TaskView]) -> String {
    let pending = tasks.iter().filter(|t| t.status == Status::Pending).count();
    let active = tasks
        .iter()
        .filter(|t| t.status == Status::InProgress)
        .count();
    let done = tasks
        .iter()
        .filter(|t| t.status == Status::Completed)
        .count();
    format!("{pending} pending, {active} in progress, {done} completed")
}

pub fn block(tasks: &[TaskView]) -> String {
    tasks.iter().map(line).collect::<Vec<_>>().join("\n")
}
