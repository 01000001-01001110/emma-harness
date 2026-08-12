//! Colour, and what happens to it on a terminal that has less of it.
//!
//! Three rules, and every one of them is here because the alternative breaks on
//! somebody else's machine.
//!
//! **Ordinary text is never given a colour.** Gruvbox's foreground is `#ebdbb2`,
//! a cream that is invisible on a white background, and Emma has no way to ask
//! the terminal what colour its background is. So body text is
//! [`Color::Reset`] — whatever the user already chose — and colour is spent only
//! on the few glyphs and labels that mark one kind of line apart from another.
//! Nothing in Emma paints a background except the answer keys on the approval
//! prompt, which set a foreground *and* a background together and are therefore
//! legible whatever is behind them.
//!
//! **Colour is never the only signal.** Every event that gets a colour also gets
//! its own glyph and its own weight — see [`super::render::Glyphs`]. A terminal
//! that renders `#b8bb26` as a muddy olive still shows `✓` where a success was,
//! and a terminal with no colour at all loses nothing but the shading.
//!
//! **The palette degrades rather than disappearing.** Truecolor gets the exact
//! hexes, a 256-colour terminal gets the nearest xterm index, and a 16-colour
//! terminal gets the named ANSI colour closest in intent. `NO_COLOR`, or a
//! stream that is not a terminal, gets none.
//!
//! # Where the colours come from now
//!
//! The accent and the secondary grey are the owner's mockup, sampled from the
//! image rather than described: `#fd548f` on the wordmark, `#e56383` on a
//! border, `#fb5797` on a meter — one hue with antialias variance, taken here as
//! the single value `#f5548f`. `notes/design-tui-fullscreen.md` §8.1 is the
//! argument; this file is where it lands.
//!
//! **The safety vocabulary did not move.** `Ok`, `Warn`, `Err` and `Info` are
//! still Gruvbox's, because a look-and-feel change is not a licence to recolour
//! the glyphs that say a command failed or that a human has to answer something.
//! The approval prompt is still amber. What changed is the decoration: the
//! accent that marks Emma's own voice, and the grey everything unimportant is
//! written in.
//!
//! **Two colours in the mockup were deliberately not adopted.** Its `#39393c`
//! borders and `#414244` empty meter segments are near-black — they read as
//! structure only because the mockup's ground is `#0d0d10`. Emma cannot know the
//! terminal's background (the rule at the top of this file), so a near-black
//! border is invisible on a dark theme and a heavy smear on a light one. Borders
//! stay on [`Role::Dim`], which is legible on both. The mockup's brighter white
//! for the `You` label and the key hints is not a role either: it is
//! [`Role::Text`] with `BOLD`, which is brighter than the surrounding grey on
//! every terminal and on every theme, which a hex is not.

use ratatui::style::{Color, Modifier, Style};

// region: How much colour this terminal has
// ---------------------------------------------------------------------------
// How much colour this terminal has
//
// Detection is pure and lives in `Level::of`, so the environment it reads is
// an argument rather than a global. `EMMA_COLORS` overrides everything, because
// the person who needs it is looking at a terminal we guessed wrong about.
// ---------------------------------------------------------------------------

/// How many colours to spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Bold and dim only.
    None,
    /// The eight ANSI colours and their bright halves.
    Ansi16,
    /// The xterm cube.
    Ansi256,
    /// 24-bit, which is what Gruvbox was drawn for.
    Truecolor,
}

impl Level {
    /// What this terminal can do, from the four things that answer the question.
    ///
    /// Ordered so the honest signals beat the hopeful ones: an explicit
    /// override, then `NO_COLOR`, then "this is not a terminal at all", then
    /// the environment's own claims. `WT_SESSION` is Windows Terminal, which
    /// does truecolor and sets neither `COLORTERM` nor a useful `TERM`.
    pub fn of(
        color: bool,
        no_color: bool,
        override_var: Option<&str>,
        colorterm: Option<&str>,
        term: Option<&str>,
        windows_terminal: bool,
    ) -> Self {
        if let Some(v) = override_var {
            match v.trim().to_ascii_lowercase().as_str() {
                "none" | "0" | "off" => return Self::None,
                "16" | "ansi" => return Self::Ansi16,
                "256" => return Self::Ansi256,
                "truecolor" | "24bit" | "full" => return Self::Truecolor,
                // An unreadable value is not permission to guess loudly.
                _ => return Self::Ansi16,
            }
        }
        if no_color || !color {
            return Self::None;
        }
        if matches!(colorterm, Some(v) if v.contains("truecolor") || v.contains("24bit")) {
            return Self::Truecolor;
        }
        if windows_terminal {
            return Self::Truecolor;
        }
        match term {
            Some(t) if t.contains("256color") => Self::Ansi256,
            Some("dumb") => Self::None,
            None => Self::Ansi16,
            Some(_) => Self::Ansi16,
        }
    }

    /// The same question, asked of the real process.
    pub fn detect(color: bool) -> Self {
        Self::of(
            color,
            std::env::var_os("NO_COLOR").is_some(),
            std::env::var("EMMA_COLORS").ok().as_deref(),
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
            std::env::var_os("WT_SESSION").is_some(),
        )
    }
}

// endregion: How much colour this terminal has

// region: The roles
// ---------------------------------------------------------------------------
// The roles
//
// Named by what a colour *means* rather than by which colour it is, so the one
// place a hex appears is the table below and a terminal that cannot render it
// substitutes at that single point.
// ---------------------------------------------------------------------------

/// What a piece of text is, as far as colour is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The user's own foreground. Never a hex — see the module doc.
    Text,
    /// Secondary: notes, borders, counts, the parts of a line nobody reads
    /// twice.
    Dim,
    /// Something worked.
    Ok,
    /// Something failed.
    Err,
    /// Something needs a human, or was refused.
    Warn,
    /// A tool, a host, a path — the nouns in a line.
    Info,
    /// The goal, the prompt, Emma itself.
    Accent,
    /// Only ever a *foreground* on an accent background: the answer keys.
    Ground,
}

/// The mockup's accent over Gruvbox's safety vocabulary, at three fidelities.
///
/// The 256 indices are the customary Gruvbox terminal mapping for the roles
/// that stayed, and the nearest cube entry for the two that moved. The ANSI
/// names are chosen by intent rather than by nearest distance — `Warn` is
/// yellow at every level even though the 24-bit yellow is nearer to some
/// oranges, because a warning that changes hue between terminals is a warning
/// somebody has to re-learn.
const fn table(role: Role) -> (u8, u8, u8, u8, Color) {
    match role {
        // Never reached: `Text` short-circuits to `Color::Reset` before this is
        // consulted. Present so the match is total and so that a future edit
        // that reaches for a foreground hex has to walk past the reason.
        Role::Text => (235, 219, 178, 223, Color::Reset),
        // Neutral rather than Gruvbox's warm `#928374`: the mockup's greys are
        // untinted, and a warm grey beside a pink accent reads as a third,
        // muddier colour rather than as an absence of one.
        Role::Dim => (143, 143, 148, 245, Color::DarkGray),
        Role::Ok => (184, 187, 38, 142, Color::LightGreen),
        Role::Err => (251, 73, 52, 167, Color::LightRed),
        Role::Warn => (250, 189, 47, 214, Color::LightYellow),
        Role::Info => (142, 192, 124, 108, Color::LightCyan),
        // The mockup's pink. Index 204 is `#ff5f87` — near enough that the two
        // are hard to tell apart side by side, which is the whole bar a 256
        // fallback has to clear.
        //
        // `LightMagenta` at 16 colours is deliberately **not** `LightRed`, even
        // though the hue is nearer: `Role::Err` owns red at that level, and the
        // accent is what a *user's own message* is drawn in. A goal line that
        // reads as an error on a 16-colour terminal is worse than a goal line
        // that reads as violet.
        Role::Accent => (245, 84, 143, 204, Color::LightMagenta),
        // Only ever a foreground on an accent background, so it is chosen for
        // contrast against the pink above rather than for resemblance to
        // anything. The mockup's own ground, which is as dark as this gets.
        Role::Ground => (13, 13, 16, 233, Color::Black),
    }
}

/// The palette in force for this run.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub level: Level,
}

impl Palette {
    pub fn new(level: Level) -> Self {
        Self { level }
    }

    /// The colour for a role, at whatever fidelity this terminal has.
    pub fn color(&self, role: Role) -> Color {
        if self.level == Level::None || role == Role::Text {
            return Color::Reset;
        }
        let (r, g, b, idx, ansi) = table(role);
        match self.level {
            Level::Truecolor => Color::Rgb(r, g, b),
            Level::Ansi256 => Color::Indexed(idx),
            _ => ansi,
        }
    }

    pub fn style(&self, role: Role) -> Style {
        Style::default().fg(self.color(role))
    }

    /// Dim is a *modifier* as well as a colour, so a terminal with no colour at
    /// all still separates a note from a line somebody has to read.
    pub fn dim(&self) -> Style {
        self.style(Role::Dim).add_modifier(Modifier::DIM)
    }

    pub fn bold(&self, role: Role) -> Style {
        self.style(role).add_modifier(Modifier::BOLD)
    }

    /// The one place a background is painted: the `[y]` / `[n]` chips on the
    /// approval prompt. Both halves are set, so it is legible on a light
    /// terminal and on a dark one, which is exactly why it is allowed here and
    /// nowhere else.
    pub fn chip(&self, role: Role) -> Style {
        if self.level == Level::None {
            // No colour to invert, so the emphasis has to come from somewhere:
            // reversed video is the one attribute every terminal has.
            return Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        Style::default()
            .fg(self.color(Role::Ground))
            .bg(self.color(role))
            .add_modifier(Modifier::BOLD)
    }
}

// endregion: The roles

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_that_says_it_does_truecolor_is_believed() {
        assert_eq!(
            Level::of(true, false, None, Some("truecolor"), Some("xterm"), false),
            Level::Truecolor
        );
        // Windows Terminal says nothing at all and does 24-bit.
        assert_eq!(
            Level::of(true, false, None, None, None, true),
            Level::Truecolor
        );
    }

    #[test]
    fn a_256_colour_terminal_gets_the_cube_and_a_bare_one_gets_sixteen() {
        assert_eq!(
            Level::of(true, false, None, None, Some("xterm-256color"), false),
            Level::Ansi256
        );
        assert_eq!(
            Level::of(true, false, None, None, Some("xterm"), false),
            Level::Ansi16
        );
    }

    #[test]
    fn no_color_and_a_pipe_both_mean_none() {
        // Two different reasons, one answer. `NO_COLOR` is a request; a pipe is
        // a fact — the bytes are going into somebody's file.
        assert_eq!(
            Level::of(true, true, None, Some("truecolor"), None, true),
            Level::None
        );
        assert_eq!(
            Level::of(false, false, None, Some("truecolor"), None, true),
            Level::None
        );
    }

    #[test]
    fn the_override_beats_every_other_signal() {
        // Somebody setting this is looking at a terminal we guessed wrong
        // about, and the guess must not get a second vote.
        assert_eq!(
            Level::of(true, true, Some("256"), None, Some("dumb"), false),
            Level::Ansi256
        );
        assert_eq!(
            Level::of(true, false, Some("none"), Some("truecolor"), None, true),
            Level::None
        );
        // And a value nobody can read degrades rather than escalating.
        assert_eq!(
            Level::of(true, false, Some("magenta please"), None, None, true),
            Level::Ansi16
        );
    }

    /// The degradation itself: what a 16-colour terminal is sent.
    ///
    /// The failure this prevents is a 24-bit hex reaching a terminal that
    /// renders it as nothing — the owner asked for a Gruvbox palette *with* a
    /// 16-colour fallback, and a fallback that is only claimed in a comment is
    /// the one that turns out not to exist.
    #[test]
    fn a_sixteen_colour_terminal_is_never_sent_a_hex_or_an_index() {
        let p = Palette::new(Level::Ansi16);
        for role in ROLES {
            match p.color(role) {
                Color::Rgb(..) => panic!("{role:?} sent 24-bit colour to a 16-colour terminal"),
                Color::Indexed(_) => {
                    panic!("{role:?} sent a 256-colour index to a 16-colour terminal")
                }
                _ => {}
            }
        }
        assert_eq!(p.color(Role::Ok), Color::LightGreen);
        assert_eq!(p.color(Role::Err), Color::LightRed);
    }

    #[test]
    fn a_256_colour_terminal_is_never_sent_a_hex() {
        let p = Palette::new(Level::Ansi256);
        for role in ROLES {
            assert!(
                !matches!(p.color(role), Color::Rgb(..)),
                "{role:?} sent 24-bit colour to a 256-colour terminal"
            );
        }
        assert_eq!(p.color(Role::Ok), Color::Indexed(142));
        // The accent's cube entry is `#ff5f87`, which is the closest the cube
        // gets to `#f5548f`. Pinned because "nearest" is an argument in a
        // comment until a number is written down.
        assert_eq!(p.color(Role::Accent), Color::Indexed(204));
    }

    #[test]
    fn truecolor_gets_the_hexes_themselves() {
        let p = Palette::new(Level::Truecolor);
        // The safety vocabulary is still Gruvbox's, and stays that way through
        // a look-and-feel change: `✓` and `✗` are not decoration.
        assert_eq!(p.color(Role::Ok), Color::Rgb(184, 187, 38));
        assert_eq!(p.color(Role::Err), Color::Rgb(251, 73, 52));
        assert_eq!(p.color(Role::Warn), Color::Rgb(250, 189, 47));
        // The accent is the mockup's, sampled from the image.
        assert_eq!(p.color(Role::Accent), Color::Rgb(245, 84, 143));
    }

    /// The accent marks the user's own words. On a 16-colour terminal it must
    /// not land on the colour that means "this failed".
    ///
    /// Written as a comparison rather than as `assert_eq!(.., LightMagenta)`
    /// because the property is the *distinctness*: a future edit that moved the
    /// accent to `LightRed` for being nearer the hue would pass a literal test
    /// on `Role::Err` and still make every goal line read as an error.
    #[test]
    fn the_accent_is_never_the_colour_that_means_failure() {
        for level in [Level::Ansi16, Level::Ansi256, Level::Truecolor] {
            let p = Palette::new(level);
            assert_ne!(
                p.color(Role::Accent),
                p.color(Role::Err),
                "at {level:?} a user's own message is drawn in the failure colour"
            );
            // …and it is not the warning colour either, which is what the
            // approval prompt speaks in.
            assert_ne!(p.color(Role::Accent), p.color(Role::Warn), "{level:?}");
        }
    }

    /// The chip is a foreground *and* a background, so the pair has to be
    /// readable. Both halves moved in this change; this is what says they moved
    /// together.
    #[test]
    fn the_answer_keys_keep_a_dark_foreground_on_the_accent() {
        for level in [Level::Ansi16, Level::Ansi256, Level::Truecolor] {
            let p = Palette::new(level);
            let chip = p.chip(Role::Accent);
            assert_eq!(chip.fg, Some(p.color(Role::Ground)), "{level:?}");
            assert_eq!(chip.bg, Some(p.color(Role::Accent)), "{level:?}");
            assert_ne!(
                chip.fg, chip.bg,
                "the answer keys are invisible at {level:?}"
            );
        }
    }

    /// The rule that keeps Emma legible on a light background: body text is the
    /// user's own foreground at every fidelity, never Gruvbox's cream.
    #[test]
    fn ordinary_text_is_never_given_a_colour_at_any_fidelity() {
        for level in [Level::None, Level::Ansi16, Level::Ansi256, Level::Truecolor] {
            assert_eq!(
                Palette::new(level).color(Role::Text),
                Color::Reset,
                "{level:?} coloured body text"
            );
        }
    }

    #[test]
    fn nothing_paints_a_background_except_the_answer_keys() {
        for level in [Level::Ansi16, Level::Ansi256, Level::Truecolor] {
            let p = Palette::new(level);
            for role in ROLES {
                assert!(
                    p.style(role).bg.is_none(),
                    "{role:?} painted a background at {level:?}"
                );
            }
            // …and the chip sets both halves, which is what makes it safe.
            let chip = p.chip(Role::Accent);
            assert!(chip.fg.is_some() && chip.bg.is_some());
        }
    }

    #[test]
    fn with_no_colour_a_chip_is_still_unmissable() {
        // Reversed video is the emphasis that survives `NO_COLOR`, and the
        // answer keys are the one thing on screen that must not be missed.
        let chip = Palette::new(Level::None).chip(Role::Accent);
        assert!(chip.add_modifier.contains(Modifier::REVERSED));
    }

    const ROLES: [Role; 8] = [
        Role::Text,
        Role::Dim,
        Role::Ok,
        Role::Err,
        Role::Warn,
        Role::Info,
        Role::Accent,
        Role::Ground,
    ];
}
