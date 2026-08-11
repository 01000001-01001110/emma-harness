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

    /// The user message that opens a goal: **the user's words, and nothing
    /// else.**
    ///
    /// This used to prepend "Work toward this goal. You have tools; use them
    /// rather than describing what you would do" and append the completion
    /// contract. Both are true of every goal, which is precisely why neither
    /// belongs here — see [`standing_contract`], which now carries them in the
    /// system prompt.
    ///
    /// The reason it moved is what a preamble does to a message that is not a
    /// work order. Typing `Hello` produced a model that ran `pwd && ls -la`
    /// and listed the task file before saying hello, at a cost of 22,083
    /// tokens — correct behaviour, given it had been handed a greeting wrapped
    /// in an instruction to go and do something. **A preamble applied to every
    /// input is an instruction the user did not write and cannot see.**
    ///
    /// What the user typed is now what the model reads.
    pub fn opening(&self) -> String {
        self.text.trim().to_string()
    }
}

/// The framing that used to be prepended to every goal, now stated once in the
/// system prompt.
///
/// Appended to the harness instructions at request assembly rather than kept in
/// a file, because the [`DoneCheck`] decides the completion half and the
/// harness cannot know which check a run selected. A contract describing a rule
/// the loop is not applying is worse than no contract — it teaches a completion
/// ritual that decides nothing — so it comes from the check, as it always did.
///
/// Being in `instructions` rather than in the first user message is also the
/// cheaper place for it: identical on every goal of every session, so it sits
/// in the stable cache prefix and is paid for once instead of riding in `query`
/// where nothing can cache it.
pub fn standing_contract(check: &dyn DoneCheck) -> String {
    format!(
        "\n\nYou work by using the tools you have rather than describing what you would do — \
         read the file, make the edit, run the command. When a request needs no tools, such as \
         a question or a greeting, simply answer it.\n\n{}",
        check.contract()
    )
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
            .trim_matches(is_decoration)
            .eq_ignore_ascii_case(MARKER)
    })
}

/// The characters a model decorates a line with, which a claim survives.
fn is_decoration(c: char) -> bool {
    matches!(c, '*' | '`' | '#' | '_' | ' ')
}

/// Takes the completion marker out of what the user reads, without taking it
/// out of what the loop reads.
///
/// **Why this exists.** [`MARKER`] is a ritual between the loop and the model,
/// and it is printed into prose a person is reading. Claude Code has no
/// completion ritual — it stops when it stops — and a session that ends every
/// answer with two shouted words reads as a machine reporting to another
/// machine, which is exactly what it is. Emma's kick needs the signal and the
/// user does not need to see it, and those are separable: `verdict` still reads
/// the model's real text, unchanged, and this is applied on the way to the
/// screen only. **Done-detection is not touched here** — deciding whether a
/// marker is the right shape of signal at all is a larger question than a
/// display fix.
///
/// **Why it is not simply a `replace` on the finished text.** Text streams. By
/// the time the whole turn is in hand it is already on the screen, so the
/// filter has to work on fragments arriving a few characters at a time.
///
/// **Why it does not simply buffer each line.** That would hold every line of
/// prose until its newline, turning a streamed paragraph into something that
/// appears a line at a time — a visible cost paid on every line to hide a
/// string that occurs on one. Instead only a line that could *still turn into*
/// the marker is held: the moment a character rules that out, everything held
/// is released and the rest of the line streams with no delay at all. In prose
/// that is a one- or two-character pause on a line beginning with `g`.
#[derive(Default)]
pub struct MarkerFilter {
    /// The start of the current line, held back because it may yet be a marker.
    held: String,
    /// This line has already been ruled out; the rest of it goes straight
    /// through.
    passing: bool,
}

impl MarkerFilter {
    /// Feed a fragment; get back what should appear on the screen now.
    pub fn push(&mut self, chunk: &str) -> String {
        let mut out = String::new();
        for c in chunk.chars() {
            if c == '\n' {
                if self.passing {
                    out.push('\n');
                } else if !claims_done(&self.held) {
                    out.push_str(&self.held);
                    out.push('\n');
                }
                // …and when it *was* the marker, the line and its newline both
                // go, so nothing is left behind where it was.
                self.held.clear();
                self.passing = false;
                continue;
            }
            if self.passing {
                out.push(c);
                continue;
            }
            self.held.push(c);
            if !could_become_marker(&self.held) {
                out.push_str(&self.held);
                self.held.clear();
                self.passing = true;
            }
        }
        out
    }

    /// End of the turn: release whatever is still held, unless it is the
    /// marker. A model that ends without a trailing newline is the ordinary
    /// case, so this is where most markers are actually caught.
    pub fn finish(&mut self) -> String {
        self.passing = false;
        let held = std::mem::take(&mut self.held);
        if claims_done(&held) {
            String::new()
        } else {
            held
        }
    }

    /// Everything, filtered, for a caller that has the whole text already.
    pub fn once(text: &str) -> String {
        let mut f = Self::default();
        let mut out = f.push(text);
        out.push_str(&f.finish());
        out
    }
}

/// Whether a partial line is still on its way to being a claim.
///
/// Deliberately generous at both ends, for the same reason [`claims_done`] is:
/// a model writing `**GOAL COMPLETE**` means the same thing, so the decoration
/// has to be tolerated *while the line is being held* or the held text is
/// released one character before the thing it was waiting for.
fn could_become_marker(line: &str) -> bool {
    let t = line.trim_start_matches(is_decoration);
    match t.get(..MARKER.len()) {
        // Long enough to have said it: it must have, and everything after must
        // be decoration.
        Some(head) => {
            head.eq_ignore_ascii_case(MARKER) && t[MARKER.len()..].chars().all(is_decoration)
        }
        // `get` also says `None` for a length that lands inside a character,
        // which only happens past the marker's own length — so that is a line
        // that is already longer than the marker and is not it.
        None if t.len() <= MARKER.len() => MARKER[..t.len()].eq_ignore_ascii_case(t),
        None => false,
    }
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

        // The opening is the user's words and nothing else. A greeting that
        // arrives wrapped in "work toward this goal" is a work order the user
        // did not type, and the model rightly obeys it.
        let goal = Goal::new("port the middleware");
        assert_eq!(goal.opening(), "port the middleware");
        assert!(!goal.opening().contains(MARKER), "the contract leaked in");
        assert!(
            !goal.opening().to_lowercase().contains("work toward"),
            "a preamble leaked into the user's message"
        );

        // The framing did not disappear — it moved to the system prompt, and it
        // still comes from the check in force rather than from here, so a
        // contract the loop is not applying cannot be taught to the model.
        let standing = standing_contract(&Never);
        assert!(standing.contains("This goal is never done."));
        assert!(!standing.contains(MARKER), "the unused contract leaked in");
        assert!(standing_contract(&MarkerClaim).contains(MARKER));

        // And the half that made `Hello` cost 22,083 tokens: tools are for
        // when the request needs them.
        assert!(standing.to_lowercase().contains("greeting"));
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

    // -----------------------------------------------------------------------
    // The marker, on its way to the screen
    //
    // Two things have to be true at once and they pull in opposite directions:
    // the loop still reads the marker, and the user never sees it. Everything
    // below is about the second one, and the first is asserted by the tests
    // above — `claims_done` is untouched and is what `verdict` reads.
    // -----------------------------------------------------------------------

    /// The whole point, in the shape the model actually produces it: an answer,
    /// a blank line, the marker, no trailing newline.
    #[test]
    fn the_marker_never_reaches_the_screen() {
        for text in [
            "Ported the middleware.\n\nGOAL COMPLETE",
            "Ported the middleware.\n\nGOAL COMPLETE\n",
            "Ported the middleware.\n\n**GOAL COMPLETE**",
            "Ported the middleware.\n\n  goal complete  \n",
        ] {
            let shown = MarkerFilter::once(text);
            assert!(
                !shown.to_lowercase().contains("goal complete"),
                "the marker was shown: {shown:?}"
            );
            assert!(
                shown.contains("Ported the middleware."),
                "the answer was eaten with it: {shown:?}"
            );
        }
    }

    /// Streamed a fragment at a time, which is how it actually arrives — and
    /// the reason the filter is a state machine rather than a `replace`.
    #[test]
    fn a_marker_split_across_fragments_is_still_caught() {
        let mut f = MarkerFilter::default();
        let mut shown = String::new();
        // Deliberately cut inside the marker, inside the word before it, and
        // between the two words of it.
        for chunk in ["Ported the mid", "dleware.\n\nGO", "AL COMP", "LETE"] {
            shown.push_str(&f.push(chunk));
        }
        shown.push_str(&f.finish());
        assert_eq!(shown, "Ported the middleware.\n\n");
    }

    /// The cost of hiding it, bounded: prose is not held back waiting to find
    /// out. A line that cannot be the marker is released the moment that is
    /// known, which is on the first character that rules it out.
    #[test]
    fn prose_is_not_delayed_by_the_filter() {
        let mut f = MarkerFilter::default();
        // Nothing about this line could be the marker after one character.
        assert_eq!(f.push("Ported"), "Ported");
        // A line that starts like it is held only until it stops being like it.
        let mut f = MarkerFilter::default();
        assert_eq!(f.push("GOAL"), "");
        assert_eq!(f.push("s are a thing"), "GOALs are a thing");
    }

    /// A sentence *about* the marker is not a claim — `claims_done` already
    /// says so — and it must not be censored either, or the model explaining
    /// its own contract to the user comes out with a hole in it.
    #[test]
    fn a_mention_of_the_marker_is_left_alone() {
        let text = "I will print GOAL COMPLETE once the tests are green.\n";
        assert_eq!(MarkerFilter::once(text), text);
        assert!(!claims_done(text));
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
