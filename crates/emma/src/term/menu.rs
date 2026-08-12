//! The command menu: what `/` shows, and when it is allowed to show it.
//!
//! # Why this exists
//!
//! `/exit` and `/quit` have always worked, and `Harness::expand_command`
//! expands `/<name>` for every command a project put in `commands/` — and
//! nothing on screen ever said so. The whole vocabulary was discoverable only
//! by reading a note that scrolled away on the first goal, or by reading the
//! source. This repository has already recorded the rule that follows from
//! that: **a command nobody can discover is not a command.**
//!
//! # The decisions, and why they are here rather than in the reader
//!
//! Everything below is pure: a filter string and a list of names in, an open
//! or closed menu out. Nothing here draws, locks or reads a terminal, because
//! the menu's behaviour is the part that can be wrong in a way nobody can see
//! from a test binary — cargo hands the tests a pipe, so the viewport is never
//! drawn while they run. What *can* be asserted is when it opens, what it
//! filters to, what closes it and what an approval does to it, so that is what
//! this file is made of.
//!
//! # The four rules
//!
//! **It opens on a leading `/` and nothing else.** Not on a `/` in the middle
//! of a sentence, and not on `/usr/bin/env` — a second `/`, or any whitespace,
//! means the user is typing a path or a command with arguments rather than
//! reaching for a list.
//!
//! **No match closes it.** A popup with nothing in it, sitting over the text
//! somebody is trying to type, is a trap. `/zzz` is ordinary input and is
//! treated as ordinary input.
//!
//! **Esc leaves the text alone.** It dismisses the menu, not the line. Clearing
//! what somebody typed because they wanted the popup out of the way is the same
//! class of theft as the drain eating a line, and the drain at least has a
//! safety argument.
//!
//! **A pending question outranks it.** While an approval is on screen the only
//! thing the keyboard is for is answering it — see `approval.rs` — so `/` is
//! then just a character, and this file is told so by its caller.

/// What is said about a harness command when the harness said nothing.
///
/// The harness exposes `command_names()` and a body to expand; there is no
/// description in it. So this line is what a project command gets, and it says
/// where the gap is rather than paraphrasing the body — a summary invented from
/// prompt text is a description of a command that nobody wrote and nobody can
/// correct.
pub const NO_DESCRIPTION: &str = "no description — the harness does not give one";

/// Shown when the project itself defines nothing.
///
/// A discovery affordance rather than an error: the answer to "what commands
/// are there" is "these two, and here is where yours would go".
///
/// Kept short on purpose. It is drawn on one row of a viewport that is often
/// sixty columns wide, and a sentence that gets cut in half loses the path —
/// the only part of it nobody can guess.
pub const NO_PROJECT_COMMANDS: &str = "this project has none of its own — .emma/commands/";

/// The placeholder in the empty input box. Here rather than in the view because
/// it names the key this file is about, and the two should not drift.
pub const PLACEHOLDER: &str = "describe a goal, or press / for commands";

/// Said about a project command a built-in shadows.
///
/// The row is kept rather than dropped, and this is the one place the menu is
/// allowed to compose a sentence — because what it composes is a fact about
/// *resolution*, not a summary of a file nobody wrote a description for. A
/// command silently missing from the list is the gap that gets diagnosed as
/// "Emma cannot see my commands"; a row saying why is one line.
pub const SHADOWED: &str = "shadowed by Emma's own command of the same name";

/// One line of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// `None` means nobody told us. See [`NO_DESCRIPTION`].
    pub about: Option<String>,
}

impl Entry {
    /// What is drawn beside the name. Never invented: a command with no
    /// description says so.
    pub fn about(&self) -> &str {
        self.about.as_deref().unwrap_or(NO_DESCRIPTION)
    }
}

/// The menu's state: the whole vocabulary, what the typed text narrowed it to,
/// and which line the arrow keys are on.
#[derive(Debug, Clone, Default)]
pub struct Menu {
    entries: Vec<Entry>,
    /// The line under the list when the project contributed nothing.
    note: Option<String>,
    open: bool,
    /// Indices into `entries`, in the order they are shown.
    matched: Vec<usize>,
    /// An index into `matched`, not into `entries`.
    selected: usize,
    /// The text the user pressed Esc at. While what they type still starts with
    /// it, the menu stays shut: reopening a popup somebody just dismissed, on
    /// their next keystroke, is worse than never having shown it.
    dismissed_at: Option<String>,
}

impl Menu {
    /// The real vocabulary of this run: Emma's own built-ins and whatever the
    /// harness loaded, in the harness's own order.
    ///
    /// Nothing is added. A menu that lists a command the harness does not have
    /// teaches, on first use, that Emma's account of itself cannot be trusted —
    /// which is the same argument the welcome screen is composed under. The
    /// built-ins come from [`crate::session_command::BUILTINS`], which is also
    /// what the parser matches on, so a row and a command cannot exist without
    /// each other.
    ///
    /// **A name collision resolves to the built-in, and the row says so.** That
    /// has been the de facto rule since the loop was written — `/exit` was
    /// matched before `Harness::expand_command` — and the alternative is a
    /// checkout being able to make Emma's own control surface unreachable.
    /// What is new is that the project's row is marked rather than duplicated:
    /// two identical rows would read as a bug, and dropping it silently would
    /// hide the collision entirely.
    pub fn for_project(commands: &[&str]) -> Self {
        let builtins = crate::session_command::BUILTINS;
        let mut entries: Vec<Entry> = builtins
            .iter()
            .map(|(name, about)| Entry {
                name: (*name).to_string(),
                about: Some((*about).to_string()),
            })
            .collect();
        for name in commands {
            let shadowed = builtins
                .iter()
                .any(|(builtin, _)| builtin.eq_ignore_ascii_case(name));
            if shadowed {
                // Marked on the built-in's own row: the name resolves to one
                // command, so it gets one row.
                if let Some(entry) = entries
                    .iter_mut()
                    .find(|e| e.name.eq_ignore_ascii_case(name))
                {
                    let about = entry.about.take().unwrap_or_default();
                    entry.about = Some(format!("{about} · this project's /{name} is {SHADOWED}"));
                }
                continue;
            }
            entries.push(Entry {
                name: (*name).to_string(),
                about: None,
            });
        }
        Self {
            entries,
            note: commands.is_empty().then(|| NO_PROJECT_COMMANDS.to_string()),
            ..Self::default()
        }
    }

    /// The typed line changed. Decide whether the menu is up, and what is in it.
    ///
    /// `suppressed` is a pending approval. It is an argument rather than a
    /// lookup so that the rule — a question outranks the menu — is a thing a
    /// test states rather than a thing a terminal has to be in front of.
    pub fn sync(&mut self, input: &str, suppressed: bool) {
        if suppressed {
            self.close();
            self.dismissed_at = None;
            return;
        }
        let Some(rest) = input.strip_prefix('/') else {
            self.close();
            self.dismissed_at = None;
            return;
        };
        // A path (`/usr/bin`) or a command that already has its arguments
        // (`/review src/lib.rs`) is somebody typing, not somebody browsing.
        if rest.contains(char::is_whitespace) || rest.contains('/') {
            self.close();
            return;
        }
        match &self.dismissed_at {
            // Still inside the text they dismissed it at.
            Some(at) if input.starts_with(at.as_str()) => {
                self.close();
                return;
            }
            _ => self.dismissed_at = None,
        }
        let filter = rest.to_ascii_lowercase();
        let matched: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.name.to_ascii_lowercase().starts_with(&filter))
            .map(|(i, _)| i)
            .collect();
        if matched.is_empty() {
            // Nothing matches, so the text is ordinary input and the menu gets
            // out of the way rather than hovering empty over it.
            self.close();
            return;
        }
        // Keep the highlight on the same command while it survives the filter,
        // so narrowing does not move the selection out from under a hand that
        // is already on Enter.
        let held = if self.open {
            self.matched.get(self.selected).copied()
        } else {
            None
        };
        self.matched = matched;
        self.selected = held
            .and_then(|i| self.matched.iter().position(|&m| m == i))
            .unwrap_or(0);
        self.open = true;
    }

    /// Esc. The menu goes; the text stays exactly as it was.
    pub fn dismiss(&mut self, input: &str) {
        self.close();
        self.dismissed_at = Some(input.to_string());
    }

    /// Shut, and forget where the arrow keys were. Used by Esc, by a submitted
    /// line and by [`super::input::LineSource::drain`] — the last of which is
    /// why this exists as its own method: a drained input box with a menu still
    /// open would be a second place for a keystroke to hide.
    pub fn close(&mut self) {
        self.open = false;
        self.matched.clear();
        self.selected = 0;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Arrow keys. Wraps, because a list of four things with a hard stop at each
    /// end is a list people press Up twice on.
    pub fn move_by(&mut self, delta: isize) {
        if !self.open || self.matched.is_empty() {
            return;
        }
        let len = self.matched.len() as isize;
        self.selected = (((self.selected as isize + delta) % len + len) % len) as usize;
    }

    /// What Enter would pick.
    pub fn selection(&self) -> Option<&Entry> {
        if !self.open {
            return None;
        }
        self.matched.get(self.selected).map(|&i| &self.entries[i])
    }

    /// What the viewport should draw, or `None` when there is nothing to draw.
    pub fn view(&self) -> Option<MenuView> {
        if !self.open {
            return None;
        }
        Some(MenuView {
            rows: self
                .matched
                .iter()
                .map(|&i| {
                    let e = &self.entries[i];
                    (e.name.clone(), e.about().to_string())
                })
                .collect(),
            selected: self.selected,
            note: self.note.clone(),
        })
    }
}

/// The menu as the viewport needs it: text, a highlight, and the note.
///
/// A snapshot rather than a borrow of [`Menu`]: the menu is owned by the reader
/// thread and the view is owned by the frame's mutex, and passing one plain
/// value between them is what keeps the two from having to lock each other.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MenuView {
    /// `(name, description)`, already resolved — see [`Entry::about`].
    pub rows: Vec<(String, String)>,
    pub selected: usize,
    pub note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Menu {
        Menu::for_project(&["review", "ship"])
    }

    fn names(m: &Menu) -> Vec<String> {
        m.view()
            .map(|v| v.rows.into_iter().map(|(n, _)| n).collect())
            .unwrap_or_default()
    }

    /// The whole feature in one assertion: a bare `/` shows everything this run
    /// can actually do.
    #[test]
    fn a_leading_slash_opens_the_whole_vocabulary() {
        let mut m = menu();
        m.sync("/", false);
        assert!(m.is_open());
        let shown = names(&m);
        // Emma's own, in `session_command`'s order, then the harness's in the
        // harness's order. The owner's complaint was that this list was two
        // rows long and neither of them did anything but leave.
        let expected: Vec<String> = crate::session_command::BUILTINS
            .iter()
            .map(|(n, _)| (*n).to_string())
            .chain(["review".to_string(), "ship".to_string()])
            .collect();
        assert_eq!(shown, expected);
    }

    /// The collision ruling, on the menu: one name, one row, and the row says
    /// which command it resolves to.
    #[test]
    fn a_project_command_a_builtin_shadows_gets_one_marked_row_not_two() {
        let mut m = Menu::for_project(&["model", "review"]);
        m.sync("/", false);
        let shown = names(&m);
        assert_eq!(
            shown.iter().filter(|n| *n == "model").count(),
            1,
            "two rows for one name: {shown:?}"
        );
        let view = m.view().unwrap();
        let (_, about) = view.rows.iter().find(|(n, _)| n == "model").unwrap();
        assert!(about.contains(SHADOWED), "{about}");
        // …and Enter on it picks Emma's, which is what actually runs.
        m.sync("/model", false);
        assert_eq!(m.selection().map(|e| e.name.clone()), Some("model".into()));
        // A project command that collides with nothing is untouched.
        let (_, review) = view.rows.iter().find(|(n, _)| n == "review").unwrap();
        assert_eq!(review, NO_DESCRIPTION);
    }

    /// Every command the parser answers has a row, and every row parses. The
    /// two lists are the same list; this is what stops the ninth command
    /// shipping undiscoverable.
    #[test]
    fn the_menu_and_the_parser_agree_on_the_vocabulary() {
        let mut m = Menu::for_project(&[]);
        m.sync("/", false);
        let shown = names(&m);
        for (name, _) in crate::session_command::BUILTINS {
            assert!(shown.iter().any(|n| n == name), "/{name} has no menu row");
        }
        for name in &shown {
            assert!(
                crate::session_command::parse(&format!("/{name}")).is_some(),
                "the menu offers /{name} and nothing parses it"
            );
        }
    }

    #[test]
    fn typing_narrows_it_and_enter_would_pick_what_is_highlighted() {
        let mut m = menu();
        m.sync("/", false);
        m.sync("/ex", false);
        assert_eq!(names(&m), ["exit"]);
        assert_eq!(m.selection().map(|e| e.name.clone()), Some("exit".into()));
    }

    /// A dead popup over the line somebody is typing is worse than no popup.
    #[test]
    fn a_filter_that_matches_nothing_closes_rather_than_trapping_anyone() {
        let mut m = menu();
        m.sync("/", false);
        m.sync("/zzz", false);
        assert!(!m.is_open());
        assert_eq!(m.selection(), None);
    }

    /// Not every `/` is a request for a menu.
    #[test]
    fn a_path_and_a_mid_sentence_slash_are_ordinary_text() {
        let mut m = menu();
        for text in [
            "/usr/bin/env",
            "look in /etc",
            "/review src/lib.rs",
            "/ ",
            "port the /api routes",
        ] {
            m.sync(text, false);
            assert!(!m.is_open(), "{text} opened the menu");
        }
    }

    /// The keystroke that must not cost anybody their sentence.
    #[test]
    fn esc_closes_the_menu_and_does_not_reopen_on_the_next_keystroke() {
        let mut m = menu();
        m.sync("/re", false);
        assert!(m.is_open());
        m.dismiss("/re");
        assert!(!m.is_open());
        // The text is untouched — this type never held it — and the next
        // character typed into the same word does not bring the popup back.
        m.sync("/rev", false);
        assert!(!m.is_open(), "a dismissed menu came back uninvited");
        // Starting a different line does.
        m.sync("port the middleware", false);
        m.sync("/", false);
        assert!(m.is_open());
    }

    /// The rule from `approval.rs`, restated here: while a question is on
    /// screen the keyboard is for answering it.
    #[test]
    fn a_pending_approval_outranks_the_menu() {
        let mut m = menu();
        m.sync("/", false);
        assert!(m.is_open());
        m.sync("/", true);
        assert!(!m.is_open(), "a menu drew over a pending question");
        // …and it stays shut for as long as the question is up.
        m.sync("/ex", true);
        assert!(!m.is_open());
    }

    #[test]
    fn the_arrows_move_the_selection_and_wrap_at_both_ends() {
        let mut m = menu();
        m.sync("/", false);
        let all = names(&m);
        assert_eq!(m.selection().unwrap().name, all[0]);
        m.move_by(1);
        assert_eq!(m.selection().unwrap().name, all[1]);
        m.move_by(-1);
        assert_eq!(m.selection().unwrap().name, all[0]);
        m.move_by(-1);
        assert_eq!(
            m.selection().unwrap().name,
            all[all.len() - 1],
            "up at the top did not wrap"
        );
        m.move_by(1);
        assert_eq!(m.selection().unwrap().name, all[0]);
    }

    /// Narrowing must not move the highlight out from under a hand already on
    /// Enter.
    #[test]
    fn the_highlight_stays_on_the_same_command_while_the_filter_narrows() {
        let mut m = Menu::for_project(&["ship", "shout"]);
        m.sync("/sh", false);
        m.move_by(1);
        assert_eq!(m.selection().unwrap().name, "shout");
        m.sync("/sho", false);
        assert_eq!(m.selection().unwrap().name, "shout");
    }

    /// The vocabulary is the harness's, not this file's.
    #[test]
    fn nothing_is_invented_and_a_command_with_no_description_says_so() {
        let mut m = Menu::for_project(&["review"]);
        m.sync("/", false);
        let view = m.view().unwrap();
        assert_eq!(
            view.rows.len(),
            crate::session_command::BUILTINS.len() + 1,
            "{view:?}"
        );
        let (_, about) = view
            .rows
            .iter()
            .find(|(n, _)| n == "review")
            .expect("the harness command is listed");
        assert_eq!(about, NO_DESCRIPTION);
        // The built-ins are Emma's own and are described properly.
        let (_, exit) = view.rows.iter().find(|(n, _)| n == "exit").unwrap();
        assert!(exit.contains("end this session"), "{exit}");
    }

    /// A project with no commands is not an empty popup: it is the two
    /// built-ins and a sentence saying where the others would go.
    #[test]
    fn a_project_with_no_commands_of_its_own_says_where_they_would_live() {
        let mut m = Menu::for_project(&[]);
        m.sync("/", false);
        let view = m.view().expect("the built-ins are always there");
        assert_eq!(view.rows.len(), crate::session_command::BUILTINS.len());
        assert_eq!(view.note.as_deref(), Some(NO_PROJECT_COMMANDS));
        assert!(view.note.unwrap().contains(".emma/commands/"));
    }

    #[test]
    fn a_project_with_commands_gets_no_note() {
        let mut m = menu();
        m.sync("/", false);
        assert_eq!(m.view().unwrap().note, None);
    }

    /// What the drain needs of it: closed, with nothing selected and nothing
    /// left to draw.
    #[test]
    fn closing_leaves_nothing_behind() {
        let mut m = menu();
        m.sync("/e", false);
        m.close();
        assert!(!m.is_open());
        assert_eq!(m.selection(), None);
        assert_eq!(m.view(), None);
    }
}
