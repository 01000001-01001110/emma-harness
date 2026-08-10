//! The goal, how "done" is decided, and the kick.
//!
//! This is the file that separates Emma from a chat client, so the reasoning is
//! written down rather than implied by the code.
//!
//! **The candidate authorities, and what each one cannot catch.**
//!
//! *The loop stops when the model stops.* Costs nothing, catches nothing. A
//! model that runs one command, reads the first error and writes "this looks
//! like a session-API mismatch" has stopped, and the goal is untouched. This is
//! the behaviour Emma exists to not have.
//!
//! *A check command exits zero.* Touches reality, and passes vacuously:
//! `cargo test` on a target with no tests, a `test -f` against a path that was
//! already there, a suite whose failing case was deleted rather than fixed. It
//! also cannot express goals with no mechanical criterion, which is most of the
//! goals a person types.
//!
//! *No open tasks remain* — `emma_tools_tasks::open_count(root)`, which answers
//! without a tool call or a rendered list. The most legible of the four,
//! because the user can watch the file in an editor while it happens, and
//! strictly better than prose because the agent had to write the claims down
//! first. **Legible is not honest**, and it fails in two specific ways: a model
//! that learns closing tasks ends the loop will close them, and a model that
//! never opened a task has an empty list from turn zero — which reads as
//! *done* before it has started.
//!
//! Both failures are bookkeeping, so the fix is to make bookkeeping
//! insufficient: use `open_count` to decide when to **stop asking**, and gate
//! the actual verdict behind something the model does not author — the
//! project's own test or build command exiting zero. That combination is the
//! intended upgrade path from [`MarkerClaim`] and it is why [`DoneCheck`] is a
//! trait. It is not built here because the MVP wanted one authority, and one
//! authority needs no rule for disagreeing.
//!
//! *The model declares done.* Cheap, works for any goal, and is a claim the
//! loop cannot verify.
//!
//! **What is implemented: the last one**, as [`MarkerClaim`]. The model must
//! end its final message with the line `GOAL COMPLETE`; stopping without it is
//! treated as stopping mid-work, and the loop says so and asks for more. That
//! is the smallest mechanism that still holds a goal across turns, which is
//! what the loop is for.
//!
//! **It is a trait, not an `if`.** [`DoneCheck`] has one implementation today
//! and the loop knows nothing else — it asks for a verdict and composes a kick
//! from whatever reason comes back. Swapping in a task-list check, or a check
//! command, or both in a priority order, is a new `impl` and a different value
//! in [`crate::agent::Setup`], not surgery on the loop. The trait is `async`
//! for exactly that reason: every alternative reads a file or runs a process.
//!
//! **What none of them catches, stated plainly:** nothing here verifies the
//! model's claim about its own work. The honest description of the guarantee is
//! "the loop will not stop *before* the model says it is finished", not "the
//! loop stops when the work is finished". Every candidate above moves that line
//! and none of them erases it, because each is ultimately a signal the model
//! itself produces.
//!
//! **The bound, because a kick that fires forever is worse than no kick.** Two
//! independent limits, enforced in the loop, and the goal ends the moment
//! either is reached:
//!
//! - `max_kicks` (default 3). A hard count for the whole goal.
//! - No two kicks in a row without tool use between them. A model that stops,
//!   is kicked, and stops again having called nothing has answered the kick;
//!   asking a third time is the loop arguing with itself.
//!
//! Both sit under the iteration, token and wall-clock budgets, so the worst
//! case is bounded by arithmetic rather than by good behaviour. The cost is
//! that a goal genuinely needing a fourth nudge stops one step early — and says
//! which limit stopped it, so the user can raise it.

// region: The goal
// ---------------------------------------------------------------------------
// The goal
//
// The text a run is held to, and the message that opens it. The opening pulls
// its contract from the check in force rather than stating one here.
// ---------------------------------------------------------------------------

/// The line [`MarkerClaim`] accepts as a claim of completion.
///
/// Deliberately two plain words on their own line rather than a token like
/// `<done/>`: the model writes it into prose it is already writing, and a human
/// reading the transcript can see the claim being made.
pub const MARKER: &str = "GOAL COMPLETE";

/// A goal held across turns.
#[derive(Debug, Clone)]
pub struct Goal {
    pub text: String,
}

impl Goal {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// The user message that opens a goal: the goal itself, plus whatever the
    /// active [`DoneCheck`] needs the model to know.
    ///
    /// The contract comes from the check rather than from here, because a
    /// contract that describes a rule the loop is not applying is worse than no
    /// contract — it teaches the model a completion ritual that decides
    /// nothing.
    ///
    /// Stated once, at the start, rather than repeated in every kick: it sits
    /// in `query`, after the cached prefix, and re-sending it on each iteration
    /// would be the same bytes at a different offset every time.
    pub fn opening(&self, check: &dyn DoneCheck) -> String {
        format!(
            "Work toward this goal. You have tools; use them rather than describing what you \
             would do.\n\nGoal:\n{}\n\n{}",
            self.text.trim(),
            check.contract()
        )
    }
}

// endregion: The goal

// region: Done-detection
// ---------------------------------------------------------------------------
// Done-detection
//
// The trait, and the one implementation. The module doc argues through the
// four candidate authorities and why this is the one that got built; what is
// here is the smallest of them plus the seam for replacing it.
// ---------------------------------------------------------------------------

/// The answer to "is this goal met?", and why not when it is not.
///
/// The reason is a `String` because it is shown to three audiences — the model
/// in the kick, the user on the terminal, the session log — and one
/// representation means all three are told the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    Yes,
    No(String),
}

/// How the loop decides a goal is met.
#[async_trait::async_trait]
pub trait DoneCheck: Send + Sync {
    /// For the log, so a transcript records which authority was in force.
    fn name(&self) -> &'static str;

    /// What the model is told about how completion will be judged.
    fn contract(&self) -> String;

    /// Consulted when the model stops. `text` is its final message.
    async fn verdict(&self, goal: &Goal, text: &str) -> Done;
}

/// The model declares completion by printing [`MARKER`] on its own line.
pub struct MarkerClaim;

#[async_trait::async_trait]
impl DoneCheck for MarkerClaim {
    fn name(&self) -> &'static str {
        "marker_claim"
    }

    fn contract(&self) -> String {
        format!(
            "When — and only when — the goal is fully met, end your final message with the \
             line:\n\n{MARKER}\n\nIf you stop without it you will be told the goal is not done \
             and asked to continue."
        )
    }

    async fn verdict(&self, _goal: &Goal, text: &str) -> Done {
        if claims_done(text) {
            Done::Yes
        } else {
            Done::No("your last message did not end with the completion line".into())
        }
    }
}

/// Whether the model claimed completion in the text it just wrote.
///
/// Tolerant about decoration — a model that emits `**GOAL COMPLETE**` meant the
/// same thing, and treating that as a non-claim would spend a kick on
/// formatting. Not tolerant about position: the marker must own its line, so a
/// sentence *about* the marker ("I will print GOAL COMPLETE when the tests
/// pass") is not a claim.
pub fn claims_done(text: &str) -> bool {
    text.lines().any(|line| {
        line.trim()
            .trim_matches(|c: char| c == '*' || c == '`' || c == '#' || c == '_' || c == ' ')
            .eq_ignore_ascii_case(MARKER)
    })
}

// endregion: Done-detection

// region: The kick
// ---------------------------------------------------------------------------
// The kick
//
// What the model is told when it stopped and the goal is not met. Composed
// here; bounded in the loop, which owns the two limits.
// ---------------------------------------------------------------------------

/// Compose the kick: why it is not done, the goal restated, and what has
/// already broken.
///
/// The last part matters. Without it the honest reading of "continue" is "try
/// again", and the first thing a model retries is the thing that just failed.
pub fn kick(goal: &Goal, why: &str, failed: &[String]) -> String {
    let mut s = format!(
        "The goal is not recorded as done: {why}. Nothing has been lost — you still have every \
         tool, and the goal is unchanged:\n\n{}\n\nContinue working on it.",
        goal.text.trim()
    );
    if !failed.is_empty() {
        s.push_str(&format!(
            "\n\nThese calls failed earlier in this goal — change the approach rather than the \
             spelling:\n- {}",
            failed.join("\n- ")
        ));
    }
    s
}

// endregion: The kick

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The two edges that decide whether a goal ends: a marker that is a claim
// versus a mention of one, and an opening that carries the contract of the
// check actually in force.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_on_its_own_line_is_a_claim_and_a_mention_is_not() {
        assert!(claims_done("all tests pass.\n\nGOAL COMPLETE"));
        assert!(claims_done("**GOAL COMPLETE**"));
        assert!(claims_done("`goal complete`"));
        // The failure this guards: a model narrating its own contract would
        // otherwise end the goal in its first sentence, before doing anything.
        assert!(!claims_done(
            "I will print GOAL COMPLETE once the tests are green."
        ));
        assert!(!claims_done("still working"));
    }

    #[tokio::test]
    async fn the_opening_carries_the_contract_of_the_check_actually_in_force() {
        // A contract describing a rule the loop is not applying teaches the
        // model a ritual that decides nothing, so the text comes from the check
        // rather than from the goal.
        struct Never;
        #[async_trait::async_trait]
        impl DoneCheck for Never {
            fn name(&self) -> &'static str {
                "never"
            }
            fn contract(&self) -> String {
                "This goal is never done.".into()
            }
            async fn verdict(&self, _g: &Goal, _t: &str) -> Done {
                Done::No("never".into())
            }
        }

        let goal = Goal::new("port the middleware");
        let opening = goal.opening(&Never);
        assert!(opening.contains("port the middleware"));
        assert!(opening.contains("This goal is never done."));
        assert!(!opening.contains(MARKER), "the unused contract leaked in");

        assert!(goal.opening(&MarkerClaim).contains(MARKER));
    }

    #[tokio::test]
    async fn the_marker_check_answers_both_ways() {
        let goal = Goal::new("g");
        assert_eq!(MarkerClaim.verdict(&goal, "GOAL COMPLETE").await, Done::Yes);
        assert!(matches!(
            MarkerClaim.verdict(&goal, "nearly there").await,
            Done::No(_)
        ));
    }

    #[test]
    fn a_kick_states_the_reason_the_goal_and_what_already_failed() {
        let text = kick(
            &Goal::new("port the middleware"),
            "the tests are still red",
            &["Bash(cargo build --release)".into()],
        );
        assert!(text.contains("the tests are still red"));
        assert!(text.contains("port the middleware"));
        assert!(text.contains("cargo build --release"));
    }
}

// endregion: Tests
