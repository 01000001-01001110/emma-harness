//! The first run.
//!
//! Emma had none. A new user saw a warning, two paths and an empty box: no
//! statement of what this is, no example of what to type, no hint that skills
//! or `/commands` exist, and nothing naming the tools that had just been loaded
//! on their behalf. Everything Emma can do was discoverable only by having been
//! told.
//!
//! This is shown **once** — see [`crate::session::first_run`], which decides
//! that from the session history rather than from a marker file, and says out
//! loud which of the two reasons it used. Everything on it is read from the run
//! that is actually starting: the harness that loaded, the tools that survived
//! selection, the commands that exist in this project. A welcome that lists
//! capabilities the run does not have is worse than no welcome, because the
//! first thing it teaches is that Emma's own account of itself cannot be
//! trusted.

use ratatui::text::{Line, Span};

use super::palette::Role;
use super::render::Skin;

/// What a new user is told, assembled from the run that is starting.
#[derive(Debug, Clone, Default)]
pub struct Welcome {
    /// How the first run was detected, in words. Shown, because "why am I
    /// seeing this?" is the first question a returning user has.
    pub reason: String,
    /// The harness that loaded: its flavour and where it was found.
    pub harness: String,
    pub tools: Vec<String>,
    pub commands: Vec<String>,
    pub skills: Vec<String>,
}

impl Skin {
    pub fn welcome(&self, w: &Welcome) -> Vec<Line<'static>> {
        // No blank row of its own at either end: the gap above and below this
        // block is a boundary `Term::welcome` declares, and a block that also
        // padded itself is how a screen ends up with two. See
        // [`super::spacing`]. The blanks *inside* are this block's own layout.
        let mut out = vec![Line::from(vec![
            Span::styled("emma", self.palette.bold(Role::Accent)),
            Span::styled(
                format!(
                    " {} a coding agent that holds a goal until it is done.",
                    self.glyphs.sep
                ),
                self.palette.style(Role::Text),
            ),
        ])];
        out.push(Line::from(Span::styled(
            "It reads and searches on its own, and asks before it writes a file, runs a \
             command, or reaches the network.",
            self.palette.dim(),
        )));
        out.push(Line::default());

        // Three things to type, and nothing else. A list of twenty options is
        // a list nobody reads; these are the ones that get somebody from here
        // to a working session.
        out.push(Line::from(Span::styled(
            "Try:",
            self.palette.bold(Role::Text),
        )));
        for (what, why) in [
            (
                "what does this project do?",
                "plain words, no syntax. The normal way in",
            ),
            (
                "add a test for the retry path and run it",
                "a goal it will hold across several tool calls",
            ),
            (
                "/exit",
                "ends the session. Ctrl-C interrupts a running goal",
            ),
        ] {
            out.push(Line::from(vec![
                Span::styled("  ", self.palette.dim()),
                Span::styled(what.to_string(), self.palette.style(Role::Accent)),
                Span::styled(format!("   {} {why}", self.glyphs.sep), self.palette.dim()),
            ]));
        }
        out.push(Line::default());

        out.push(self.fact("harness", &w.harness));
        if !w.tools.is_empty() {
            out.push(self.list("tools", &w.tools));
        }
        if !w.commands.is_empty() {
            out.push(
                self.list(
                    "commands",
                    &w.commands
                        .iter()
                        .map(|c| format!("/{c}"))
                        .collect::<Vec<_>>(),
                ),
            );
        }
        if !w.skills.is_empty() {
            out.push(self.list("skills", &w.skills));
        }
        if w.commands.is_empty() && w.skills.is_empty() {
            // Said rather than left silent: a user who has read about commands
            // and skills and sees neither should learn that this project has
            // none, not wonder whether Emma has them.
            out.push(Line::from(Span::styled(
                "  this project's harness defines no commands or skills yet. They live in \
                 commands/ and skills/ beside its instructions"
                    .to_string(),
                self.palette.dim(),
            )));
        }
        out.push(Line::from(Span::styled(
            format!("  shown once {} {}", self.glyphs.sep, w.reason),
            self.palette.dim(),
        )));
        out
    }

    fn fact(&self, label: &str, value: &str) -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("  {label:<9}"), self.palette.dim()),
            Span::styled(value.to_string(), self.palette.style(Role::Info)),
        ])
    }

    fn list(&self, label: &str, items: &[String]) -> Line<'static> {
        self.fact(label, &items.join("  "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::palette::{Level, Palette};
    use crate::term::render::{plain, UNICODE};

    fn text(w: &Welcome) -> String {
        let skin = Skin::new(Palette::new(Level::None), UNICODE);
        skin.welcome(w)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_new_user_is_told_what_this_is_what_to_type_and_how_to_leave() {
        let out = text(&Welcome {
            reason: "no session directory yet".into(),
            harness: "claude at C:\\src\\emma\\.claude".into(),
            tools: vec!["Read".into(), "Bash".into()],
            commands: vec!["review".into()],
            skills: vec!["tdd".into()],
        });
        assert!(out.contains("coding agent"), "{out}");
        assert!(out.contains("asks before it writes"), "{out}");
        assert!(out.contains("/exit"), "{out}");
        assert!(out.contains("Ctrl-C"), "{out}");
        // The run's own capabilities, not a brochure's.
        assert!(out.contains("Read  Bash"), "{out}");
        assert!(out.contains("/review"), "{out}");
        assert!(out.contains("tdd"), "{out}");
        // And why they are seeing it, so a returning user can tell whether
        // something is wrong.
        assert!(out.contains("shown once"), "{out}");
        assert!(out.contains("no session directory yet"), "{out}");
    }

    /// **The block does not pad itself.** The gap above and below the welcome
    /// is a boundary `Term::welcome` declares, and one that also shipped its
    /// own blank row would put two on screen wherever the block that follows
    /// declares the same boundary — which is the whole of
    /// [`crate::term::spacing`]. The blanks *between* its sections stay: those
    /// are this block's layout, not the gap around it.
    #[test]
    fn the_welcome_leaves_the_gap_around_itself_to_whoever_is_placing_it() {
        let skin = Skin::new(Palette::new(Level::None), UNICODE);
        let out = skin.welcome(&Welcome {
            harness: "claude".into(),
            ..Welcome::default()
        });
        assert!(
            !plain(&out[0]).trim().is_empty(),
            "it opens with a blank row"
        );
        assert!(
            !plain(out.last().unwrap()).trim().is_empty(),
            "it ends with a blank row"
        );
        assert!(
            out.iter().any(|l| plain(l).trim().is_empty()),
            "its own sections stopped being separated"
        );
    }

    #[test]
    fn a_project_with_no_commands_or_skills_is_told_so_rather_than_left_guessing() {
        let out = text(&Welcome {
            reason: "r".into(),
            harness: "claude".into(),
            tools: vec!["Read".into()],
            ..Welcome::default()
        });
        assert!(out.contains("defines no commands or skills"), "{out}");
    }
}
