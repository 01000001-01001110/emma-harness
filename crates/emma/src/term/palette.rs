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
//! # The colours are data; the three rules above are not
//!
//! Which hex a role has comes from a [`Theme`](super::theme::Theme) — the
//! built-in one, or a file somebody wrote (`notes/design-themes.md`). What a
//! theme cannot do is reach any of the three rules. It never colours
//! [`Role::Text`], it never paints a lone background, and it is never consulted
//! at all on a terminal with no colour to spend. Those are enforced twice on
//! purpose: once by the loader, which refuses the keys by name, and once here
//! in [`Palette::color`], which returns before the theme is read. The loader
//! can only refuse data it has thought of; this function cannot be given data
//! at all.
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

use super::theme::{Pair, Theme};

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
// place a hex appears is the theme, and a terminal that cannot render it
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

// ---------------------------------------------------------------------------
// Where the eight values live, and why the arguments for them stayed here
//
// The numbers are in [`super::theme`] now: the whole of the theme feature is
// that this file reads a role's three values out of a `Theme` rather than out
// of a `const fn`. The *arguments* did not go with them, because they are
// arguments about what a role means, and the person most likely to reverse one
// by accident is somebody editing a theme file who never opens the loader:
//
// - `Dim` is a neutral grey rather than Gruvbox's warm `#928374`: the mockup's
//   greys are untinted, and a warm grey beside a pink accent reads as a third,
//   muddier colour rather than as an absence of one.
// - `Accent` is the mockup's pink, and its 256-colour index 204 is `#ff5f87` —
//   near enough that the two are hard to tell apart side by side, which is the
//   whole bar a 256 fallback has to clear.
// - `Accent` at sixteen colours is deliberately **not** `LightRed`, even though
//   the hue is nearer: `Role::Err` owns red at that level, and the accent is
//   what a *user's own message* is drawn in. A goal line that reads as an error
//   on a 16-colour terminal is worse than a goal line that reads as violet.
//   This is also why a theme's sixteen-colour name is inherited from the
//   built-in role rather than derived — nearest distance is precisely the
//   metric that makes that mistake, and it cannot be told about intent.
// - `Warn` is yellow at every level even though the 24-bit yellow is nearer to
//   some oranges: a warning that changes hue between terminals is a warning
//   somebody has to re-learn.
// - `Ground` is only ever a foreground on an accent background, so it is chosen
//   for contrast against the pink rather than for resemblance to anything.
// - `Text` has no value here at all, and that is the point. It short-circuits
//   to `Color::Reset` in `color` below, *before* the theme is consulted, so no
//   theme can reach it however the file was written. The loader refuses
//   `roles.text` by name as well — see the module doc for why a cream
//   foreground on somebody else's white terminal reads as Emma being broken
//   rather than as the theme being wrong.
// ---------------------------------------------------------------------------

/// The palette in force for this run: how much colour, and which colours.
///
/// **`Copy`, and that is load-bearing.** Roughly thirty call sites pass a
/// `Palette` — and the [`Skin`](super::render::Skin) that holds one — by value,
/// and the whole of `term/` stores them in widgets. A theme is resolved to
/// fixed-size data at load precisely so this stays true: a `Theme` holding
/// `String`s would end `Copy` and cascade through every signature in the module.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub level: Level,
    pub theme: Theme,
}

impl Palette {
    /// The built-in theme at this fidelity — what Emma looks like out of the box.
    pub fn new(level: Level) -> Self {
        Self {
            level,
            theme: super::theme::BUILTIN,
        }
    }

    /// A resolved theme at this fidelity.
    ///
    /// **The level is an argument and is never re-decided here.** [`Level::of`]
    /// reads the environment once, at startup; a theme swapped in mid-session
    /// cannot turn a sixteen-colour terminal into a truecolor one and certainly
    /// cannot overturn `NO_COLOR`. A theme says *which* colours; only the
    /// terminal says *how many*.
    pub fn with_theme(level: Level, theme: Theme) -> Self {
        Self { level, theme }
    }

    /// The colour for a role, at whatever fidelity this terminal has.
    ///
    /// **The first line is the whole of the safety argument, and its position
    /// is the argument.** `Level::None` — which is what `NO_COLOR`, a pipe and
    /// `--print` all produce — and `Role::Text` both return before the theme is
    /// read, so there is no path on which a theme is consulted and either of
    /// them applies. That is stronger than a check inside the loader, because a
    /// check can be forgotten by whoever adds the next method here.
    pub fn color(&self, role: Role) -> Color {
        if self.level == Level::None || role == Role::Text {
            return Color::Reset;
        }
        match self.level {
            Level::Truecolor => {
                let (r, g, b) = self.theme.rgb(role);
                Color::Rgb(r, g, b)
            }
            Level::Ansi256 => Color::Indexed(self.theme.indexed(role)),
            _ => self.theme.ansi16(role),
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

    /// One of the two places a background is painted: the `[y]` / `[n]` chips
    /// on the approval prompt. Both halves are set, so it is legible on a light
    /// terminal and on a dark one, which is exactly why it is allowed here and
    /// nowhere else.
    ///
    /// **Its halves are roles, not a stored pair, because the background is the
    /// caller's argument** — the prompt draws its answer key on
    /// [`Role::Accent`] and its refusal on [`Role::Warn`], and a pair with a
    /// fixed background could not say both. So the chip is themed the way
    /// everything else is: through `color`, one role at a time. The schema's
    /// `pairs.chip` exists for the halves a role cannot supply; the pair a
    /// theme genuinely has to name for itself is [`Palette::band`]'s.
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

    /// The other one: the sidebar's selected-session band, border to border.
    ///
    /// Measured off `notes/mockup-tui.png` (2026-08-13) — accent text on a
    /// barely-raised near-black, rgb(25,27,30) against the rgb(13,15,19)
    /// ground — and deliberately *not* [`Palette::chip`]'s dark-on-pink, which
    /// an earlier draft reused and which reads as a second approval prompt.
    /// It lived in `sidebar.rs` behind a note saying it was there only because
    /// this file was not that workstream's to grow. It is this one's, and a
    /// second private colour table outside the one substitution point is
    /// exactly the drift a theme cannot be applied to.
    ///
    /// Both halves are always set, which is the invariant that makes a
    /// background safe at all (module doc), and with no colour the pair
    /// degrades to reversed video exactly as the chip does — the selection is
    /// never carried by colour alone, and the `>` in the sidebar's lead
    /// survives everything.
    ///
    /// **The two fidelities that are not a hex, and why they differ.** The 256
    /// index is *derived* from the pair's own hex, because that is mechanical:
    /// [`nearest_index`] puts rgb(25,27,30) on 234, the greyscale ramp's
    /// `#1c1c1c` and the nearest step to the sampled band. The sixteen-colour
    /// background is *not* derived, for the reason a theme's `ansi16` is
    /// inherited rather than computed: nearest distance on a near-black picks
    /// black, which is the terminal's own background on the machines this band
    /// exists for, and a band the same colour as the ground is not a band.
    /// `DarkGray` is chosen by intent and is as subtle as sixteen colours get.
    /// The foreground at that fidelity comes from [`Role::Accent`], which a
    /// theme *can* name — the half with a role behind it stays themed.
    pub fn band(&self) -> Style {
        if self.level == Level::None {
            return Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        let (fg, bg) = self.theme.pair(Pair::Selection);
        match self.level {
            Level::Truecolor => Style::default()
                .fg(Color::Rgb(fg.0, fg.1, fg.2))
                .bg(Color::Rgb(bg.0, bg.1, bg.2)),
            Level::Ansi256 => Style::default()
                .fg(Color::Indexed(nearest_index(fg)))
                .bg(Color::Indexed(nearest_index(bg))),
            _ => Style::default()
                .fg(self.color(Role::Accent))
                .bg(BAND_ANSI16_BG),
        }
    }
}

/// The sixteen-colour background of the selection band. A named colour, chosen
/// by intent — see [`Palette::band`] for why this one is not computed from the
/// pair's hex the way the 256 index is.
const BAND_ANSI16_BG: Color = Color::DarkGray;

/// The nearest xterm-256 entry to a 24-bit colour, by squared RGB distance over
/// the 6×6×6 cube (indices 16–231, levels 0/95/135/175/215/255) and the 24-step
/// greyscale ramp (232–255, value `8 + 10i`).
///
/// **Only pairs come through here, and only for their 256-colour half.** A
/// role's index is declared by the theme, because `notes/design-themes.md` §2.3
/// measured this function against the seven hexes Emma ships and found it
/// reproduces five of them exactly and disagrees with two by a shade of the
/// same hue — good enough to derive from, not good enough to overwrite a
/// deliberate choice with. A pair has no role to inherit an index from, so
/// here derivation is the only honest answer available.
///
/// The consequence worth knowing: where a pair's half names a role whose index
/// the theme *declared* against the derivation, the band will use the derived
/// one. For the built-in they agree — accent derives to the 204 it declares,
/// which is what the test below pins.
fn nearest_index(rgb: (u8, u8, u8)) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    fn dist(a: (u8, u8, u8), b: (u8, u8, u8)) -> i32 {
        let d = |x: u8, y: u8| {
            let d = i32::from(x) - i32::from(y);
            d * d
        };
        d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
    }
    let cube = LEVELS.iter().enumerate().flat_map(|(r, &rv)| {
        LEVELS.iter().enumerate().flat_map(move |(g, &gv)| {
            LEVELS
                .iter()
                .enumerate()
                .map(move |(b, &bv)| ((16 + 36 * r + 6 * g + b) as u8, (rv, gv, bv)))
        })
    });
    let ramp = (0..24u8).map(|i| {
        let v = 8 + 10 * i;
        (232 + i, (v, v, v))
    });
    // A tie goes to the lower index — `min_by_key` keeps the first — so the
    // cube wins over the ramp where both land on the same distance, which is
    // the same order the two structures have in the palette itself.
    cube.chain(ramp)
        .min_by_key(|&(_, candidate)| dist(rgb, candidate))
        .map(|(idx, _)| idx)
        .expect("the cube and the ramp are both non-empty")
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
    ///
    /// **This one keeps its teeth under a theme, and by construction rather
    /// than by fixture.** A theme carries a value for every role including
    /// `Text`; removing the `role == Role::Text` short-circuit in
    /// [`Palette::color`] would return that value, and at truecolor and 256 the
    /// result is a `Color::Rgb`/`Color::Indexed`, neither of which can ever
    /// equal `Color::Reset`. So the assertion below goes red for any theme at
    /// all, not merely for one the fixture happens to have chosen.
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

    /// §2.4's widening: *the named pairs*, now that there are two of them.
    /// Every role is a lone foreground and paints nothing; both pairs set both
    /// halves, which is the invariant that makes a background safe at all.
    #[test]
    fn nothing_paints_a_background_except_the_named_pairs() {
        for level in [Level::Ansi16, Level::Ansi256, Level::Truecolor] {
            let p = Palette::new(level);
            for role in ROLES {
                assert!(
                    p.style(role).bg.is_none(),
                    "{role:?} painted a background at {level:?}"
                );
            }
            for (name, pair) in [("chip", p.chip(Role::Accent)), ("band", p.band())] {
                assert!(
                    pair.fg.is_some() && pair.bg.is_some(),
                    "the {name} set only one half at {level:?}"
                );
                assert_ne!(pair.fg, pair.bg, "the {name} is invisible at {level:?}");
            }
        }
    }

    #[test]
    fn with_no_colour_a_chip_is_still_unmissable() {
        // Reversed video is the emphasis that survives `NO_COLOR`, and the
        // answer keys are the one thing on screen that must not be missed.
        let chip = Palette::new(Level::None).chip(Role::Accent);
        assert!(chip.add_modifier.contains(Modifier::REVERSED));
    }

    /// **`NO_COLOR` wins over a theme by construction, not by a check.**
    ///
    /// `Level::None` is what `NO_COLOR`, a pipe and `--print` all produce, and
    /// [`Palette::color`] returns before the theme is read on that path. The
    /// mutation this must not survive is moving the theme lookup above the
    /// short-circuit: every role would then come back as a hex, an index or a
    /// named colour, and none of those is `Color::Reset`.
    ///
    /// The pairs are checked here too, because they are the only two places a
    /// background exists and they have their own early return to lose.
    #[test]
    fn with_no_colour_nothing_a_theme_could_say_reaches_the_screen() {
        let p = Palette::new(Level::None);
        for role in ROLES {
            assert_eq!(p.color(role), Color::Reset, "{role:?} was coloured anyway");
            assert!(p.style(role).bg.is_none(), "{role:?} painted a background");
        }
        for (name, pair) in [("chip", p.chip(Role::Accent)), ("band", p.band())] {
            assert_eq!(pair.fg, None, "the {name} carried a foreground colour");
            assert_eq!(pair.bg, None, "the {name} carried a background colour");
            assert!(
                pair.add_modifier.contains(Modifier::REVERSED),
                "the {name} lost the one emphasis that survives no colour"
            );
        }
    }

    /// The selection band, at all four fidelities, pinned to what shipped.
    ///
    /// The literals are the numbers `sidebar.rs::band` held before this pair
    /// moved into the palette — the whole point of the move being that nothing
    /// about it changed except where it lives. `Color::Rgb(25, 27, 30)` is the
    /// sampled band; `Indexed(234)` is what [`nearest_index`] makes of it, and
    /// `Indexed(204)` is the accent's own cube entry, so the derived pair and
    /// the declared role agree for the theme Emma ships.
    #[test]
    fn the_selection_band_is_the_measured_pair_at_every_fidelity() {
        let truecolor = Palette::new(Level::Truecolor).band();
        assert_eq!(truecolor.fg, Some(Color::Rgb(245, 84, 143)));
        assert_eq!(truecolor.bg, Some(Color::Rgb(25, 27, 30)));

        let indexed = Palette::new(Level::Ansi256).band();
        assert_eq!(indexed.fg, Some(Color::Indexed(204)));
        assert_eq!(indexed.bg, Some(Color::Indexed(234)));

        let named = Palette::new(Level::Ansi16).band();
        assert_eq!(named.fg, Some(Color::LightMagenta));
        assert_eq!(named.bg, Some(Color::DarkGray));

        let none = Palette::new(Level::None).band();
        assert_eq!(
            none,
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        );

        // …and it is not the approval chip, at any fidelity. This is the
        // regression the owner reported by eye as "not formatted like the
        // image", and it is a property rather than a literal on purpose.
        for level in [Level::Ansi16, Level::Ansi256, Level::Truecolor] {
            let p = Palette::new(level);
            assert_ne!(p.band(), p.chip(Role::Accent), "at {level:?}");
        }
    }

    /// **The safety of the whole theme change: with the built-in theme, every
    /// role at every fidelity is the exact colour that shipped before the
    /// values moved out of this file.**
    ///
    /// Written as a full table rather than as spot checks because the failure
    /// it guards against is a single value drifting during the move — which no
    /// property test can see, since every property here (degradation, accent ≠
    /// error, no lone background) would still hold of a subtly different
    /// palette. Somebody who changes a colour on purpose edits this table and
    /// says so; nobody changes one by accident.
    #[test]
    fn the_built_in_theme_renders_exactly_what_shipped() {
        let expected = [
            // (role, truecolor, 256, 16)
            (Role::Text, Color::Reset, Color::Reset, Color::Reset),
            (
                Role::Dim,
                Color::Rgb(143, 143, 148),
                Color::Indexed(245),
                Color::DarkGray,
            ),
            (
                Role::Ok,
                Color::Rgb(184, 187, 38),
                Color::Indexed(142),
                Color::LightGreen,
            ),
            (
                Role::Err,
                Color::Rgb(251, 73, 52),
                Color::Indexed(167),
                Color::LightRed,
            ),
            (
                Role::Warn,
                Color::Rgb(250, 189, 47),
                Color::Indexed(214),
                Color::LightYellow,
            ),
            (
                Role::Info,
                Color::Rgb(142, 192, 124),
                Color::Indexed(108),
                Color::LightCyan,
            ),
            (
                Role::Accent,
                Color::Rgb(245, 84, 143),
                Color::Indexed(204),
                Color::LightMagenta,
            ),
            (
                Role::Ground,
                Color::Rgb(13, 13, 16),
                Color::Indexed(233),
                Color::Black,
            ),
        ];
        for (role, truecolor, indexed, named) in expected {
            for (level, want) in [
                (Level::Truecolor, truecolor),
                (Level::Ansi256, indexed),
                (Level::Ansi16, named),
                (Level::None, Color::Reset),
            ] {
                assert_eq!(
                    Palette::new(level).color(role),
                    want,
                    "{role:?} at {level:?} is not what shipped"
                );
            }
        }
    }

    /// `Palette::new` and `Palette::with_theme` are the same thing when the
    /// theme is the built-in one — which is what keeps the ~30 call sites that
    /// say `Palette::new(Level::…)` honest, and what makes a mid-session swap
    /// back to the default a no-op rather than an approximation.
    ///
    /// And the level is carried, not re-decided: `with_theme` has no way to
    /// detect anything, so a theme cannot promote a terminal's fidelity.
    #[test]
    fn a_theme_chooses_colours_and_never_how_many_of_them_there_are() {
        for level in [Level::None, Level::Ansi16, Level::Ansi256, Level::Truecolor] {
            let swapped = Palette::with_theme(level, super::super::theme::BUILTIN);
            assert_eq!(swapped.level, level, "the swap moved the fidelity");
            for role in ROLES {
                assert_eq!(swapped.color(role), Palette::new(level).color(role));
            }
            assert_eq!(swapped.band(), Palette::new(level).band());
        }
    }

    /// `Palette` and everything holding one are passed by value across `term/`.
    /// A theme that ended `Copy` would cascade through every signature in the
    /// module, which is why it is resolved to fixed-size data at load.
    #[test]
    fn a_palette_is_still_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<Palette>();
        assert_copy::<Theme>();
        assert_copy::<Level>();
    }

    /// The 256-colour derivation, against the seven hexes
    /// `notes/design-themes.md` §2.3 measured — the one measured claim in that
    /// document, reproduced here so it is a receipt rather than a citation.
    ///
    /// Five reproduce the shipping index exactly. Two do not, and the document
    /// says so: `Err` derives to 203 where the customary Gruvbox terminal
    /// mapping is 167, and `Dim` to 246 where it is 245 — one greyscale step.
    /// Both divergences are pinned here rather than smoothed over, because the
    /// reason the built-in theme *declares* those two indices is that this
    /// function would otherwise quietly change what Emma looks like.
    #[test]
    fn the_256_derivation_reproduces_the_measured_table() {
        assert_eq!(nearest_index((245, 84, 143)), 204, "Accent");
        assert_eq!(nearest_index((184, 187, 38)), 142, "Ok");
        assert_eq!(nearest_index((250, 189, 47)), 214, "Warn");
        assert_eq!(nearest_index((142, 192, 124)), 108, "Info");
        assert_eq!(nearest_index((13, 13, 16)), 233, "Ground");
        assert_eq!(nearest_index((251, 73, 52)), 203, "Err diverges from 167");
        assert_eq!(nearest_index((143, 143, 148)), 246, "Dim diverges from 245");
        // The band's own background, which is the value this function exists
        // for: the greyscale ramp's `#1c1c1c`, one step from the sampled band.
        assert_eq!(nearest_index((25, 27, 30)), 234, "the selection band");
        // The ends of both structures, so a cube-index or ramp-offset error
        // cannot hide between the samples above.
        assert_eq!(nearest_index((0, 0, 0)), 16);
        assert_eq!(nearest_index((255, 255, 255)), 231);
        assert_eq!(nearest_index((128, 128, 128)), 244);
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
