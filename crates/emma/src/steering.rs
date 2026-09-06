//! Steering: what a person types while a goal is already running.
//!
//! **The rule this feature had to survive, not weaken.** `approval.rs` drains
//! the line channel before every prompt, because a `y` typed before a question
//! existed once became a goal, and a `y` left over from one question must never
//! answer the next. That drain is the reason nothing read the keyboard while a
//! goal ran. Steering does not reopen it: the queue here is a *second* channel
//! that no prompt ever reads. `Approvals::ask` and `Approvals::read_line` read
//! `LineSource` and only `LineSource`; nothing in this file is reachable from
//! either. So a queued steer cannot answer an approval by construction rather
//! than by care.
//!
//! The other direction is a decision rather than a construction, and it is
//! taken at the seam in `term/input.rs`: while a question is on screen, typed
//! input is the ANSWER path, so nothing is queued at all. `mid_goal` is handed
//! `prompt_pending` for exactly that ruling, and it is the one guard that
//! stands between an answer and this queue.
//!
//! **Why a side queue is now right when `session_command.rs` argued it was
//! wrong.** That argument had three legs. The first, "the obvious queue is the
//! one that must not be used", is the reason this is not the `mpsc` channel.
//! The second, "a side queue needs a dispatch point inside the loop", was a
//! statement of cost, and the cost is now paid: [`crate::agent::Agent::run_goal`]
//! drains at the top of each iteration, which is a boundary that already exists.
//! The third, "deferred execution is its own hazard", is why the boundary set
//! is small and argued command by command in [`crate::session_command::mid_goal`],
//! and why `/clear` and `/resume` are still refused.

use std::sync::{Arc, Mutex, OnceLock};

use crate::session_command::SessionCommand;

/// One thing a person typed mid-goal, waiting for the next turn boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Steer {
    /// Ordinary words. They become attributed user content in the next turn.
    Text(String),
    /// A boundary-safe built-in, with the line it was typed as, so the receipt
    /// can name it.
    Command(Box<SessionCommand>),
}

/// The queue itself: a handle, cloneable, shared between the reader thread that
/// fills it and the loop that empties it.
///
/// A `std::sync::Mutex` rather than tokio's because both sides hold it for the
/// length of a `push` or a `take` and neither awaits inside it. The reader is a
/// plain OS thread and cannot await at all.
#[derive(Clone, Default)]
pub struct Steering(Arc<Mutex<Vec<Steer>>>);

impl Steering {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one, at the back. Order is the order they were typed: a person who
    /// types two corrections means the second to follow the first.
    pub fn push(&self, steer: Steer) {
        self.lock().push(steer);
    }

    /// Everything waiting, and the queue is empty afterwards.
    pub fn take(&self) -> Vec<Steer> {
        std::mem::take(&mut *self.lock())
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Steer>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The one queue a real run uses.
///
/// A process global for the same reason [`crate::approval::publish_mode`] is
/// one: the two ends are a detached OS thread reading the keyboard and a loop
/// several frames down `main`'s stack, and threading a handle between them
/// would touch every file in between. Tests never touch it (they build their
/// own [`Steering`] and hand it to the agent), so nothing here is shared state
/// between test cases.
pub fn global() -> Steering {
    static QUEUE: OnceLock<Steering> = OnceLock::new();
    QUEUE.get_or_init(Steering::new).clone()
}

/// What the transcript says when a line is queued, and what it promises.
///
/// Two wordings because two things are true, and the difference is which engine
/// is running: Emma's own loop injects at the next turn, and the claude engine
/// cannot be reached mid-run at all, so its queue drains when the goal ends.
/// A row that promised "the next turn" on a claude goal would be a promise the
/// code does not keep.
pub fn queued_notice(text: &str, at_the_next_turn: bool) -> String {
    let when = if at_the_next_turn {
        "queued for the next turn"
    } else {
        "queued for after this goal"
    };
    format!("{when}: {text}")
}

/// The bracketed register injected steering arrives in.
///
/// **Attributed, in the same style [`crate::goal::Goal::opening_turn`] uses for
/// hook context, and for the mirror-image reason.** There, text the user did not
/// write must not read as theirs. Here, text the user *did* write arrives in a
/// place they could not have written it, after tool results, inside a turn the
/// loop composed, and a model that cannot tell the difference reads a mid-task
/// correction as part of a tool's output.
///
/// One string rather than a second content block, and it rides the user turn
/// that is already there, because two user turns in a row is a 400 and an
/// assistant turn would be words put in the model's mouth. See
/// [`crate::session::append_user_text`], which is the rule and is shared with
/// the fold.
pub fn attributed(lines: &[String]) -> String {
    let mut s = String::from(
        "[The user typed this while you were working, after the results above. It is them \
         speaking mid-task: treat it as their current instruction and adjust.]\n",
    );
    for line in lines {
        s.push_str(line.trim());
        s.push('\n');
    }
    // The trailing newline goes: what this is appended to already separates
    // itself, and a turn ending in blank lines is bytes paid for twice.
    s.truncate(s.trim_end().len());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_keeps_the_order_it_was_typed_in() {
        let q = Steering::new();
        assert!(q.is_empty());
        q.push(Steer::Text("first".into()));
        q.push(Steer::Text("second".into()));
        assert_eq!(
            q.take(),
            vec![Steer::Text("first".into()), Steer::Text("second".into())]
        );
        // Taking empties it, so a second drain at the next turn injects nothing.
        assert!(q.is_empty());
        assert!(q.take().is_empty());
    }

    #[test]
    fn a_handle_is_the_same_queue() {
        let q = Steering::new();
        let other = q.clone();
        other.push(Steer::Text("use tokio".into()));
        assert_eq!(q.take(), vec![Steer::Text("use tokio".into())]);
    }

    #[test]
    fn the_injection_is_attributed_and_carries_every_line_in_order() {
        let text = attributed(&["use tokio".into(), "and skip the docs".into()]);
        assert!(text.contains("while you were working"), "{text:?}");
        assert!(text.find("use tokio").unwrap() < text.find("and skip").unwrap());
        assert!(!text.ends_with('\n'), "{text:?}");
    }

    #[test]
    fn the_queued_row_says_which_promise_is_being_made() {
        assert!(queued_notice("go", true).contains("the next turn"));
        assert!(queued_notice("go", false).contains("after this goal"));
    }
}
