//! A theme is a file of numbers, and this module is where it stops being one.
//!
//! [`super::palette`] argues that the one place a hex may appear is its table;
//! this file is that table made readable from disk, and every rule below exists
//! so that letting a stranger choose the colours cannot cost more than colour.
//! The themes design is the argument in full; what follows is the part
//! that has to be true of the code.
//!
//! **Resolution happens once, at load, into plain `Copy` data.** [`Theme`] is a
//! fixed-size array of resolved entries, not a map of `String`s, because
//! `Palette` and `Skin` are `Copy` and are passed by value into roughly thirty
//! call sites. A theme that held a `String` would end that and cascade through
//! every signature in `term/`. Strings are parsed here and discarded here.
//!
//! **Declare one, derive one, inherit one.** A role declares its hex; the
//! 256-colour index is derived from it by nearest neighbour over the xterm cube
//! ([`derive_index`]); the 16-colour name is *inherited from the built-in role*
//! unless the author declares it. The asymmetry is the measured one: derivation
//! reproduces five of the seven current indices exactly, and at sixteen colours
//! the same arithmetic makes `Accent` `LightRed`, which is how every goal line
//! comes to read as an error. Intent cannot be computed, so it is either
//! declared by somebody who knows or kept from the role that already had it
//! right.
//!
//! **Two things a theme may not do, and both are about a screen nobody here can
//! see.** It may not set [`Role::Text`], because a theme author has one
//! terminal and their foreground is what makes Emma unusable for the person
//! with the opposite background — and that person reads it as Emma being
//! broken. And it may not set a background on its own: backgrounds are declared
//! as [`Pair`]s, fg and bg together, because a pair is legible whatever is
//! behind it and a lone background is a colour against an unknown one.
//!
//! **Failure costs colour and never the boot.** [`load`] returns notices, never
//! an error: a bad hex loses one role, an unreadable file loses the theme, and
//! either way Emma says so in words. This follows `harness/src/statusline.rs`
//! ("cosmetic → note it and draw the built-in") rather than the harness's
//! refuse-to-start rule, because every distinction a theme touches is made a
//! second time in glyphs, so a theme that fails entirely costs shading.

use std::path::{Path, PathBuf};

use ratatui::style::Color;

use super::palette::Role;

// region: The resolved theme
// ---------------------------------------------------------------------------
// The resolved theme
//
// Everything below the loader is plain data: three fidelities per role and two
// pairs, `Copy`, with no allocation anywhere in it. This is the half `palette`
// consumes, and it is deliberately the half that cannot fail.
// ---------------------------------------------------------------------------

/// The two places Emma paints a background. Named rather than open-ended: a
/// third one has to be added here, and needing an entry in this enum is a
/// useful speed bump on "just paint a background".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pair {
    /// The `[y]`/`[n]` answer keys on the approval prompt.
    Chip,
    /// The sidebar's selected-session band.
    Selection,
}

/// One role at all three fidelities. Which one is used is [`super::palette`]'s
/// decision, made after this — a theme supplies the values and never chooses
/// between them, which is what keeps `NO_COLOR` and a 16-colour terminal out of
/// a theme author's reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    rgb: (u8, u8, u8),
    idx: u8,
    ansi: Color,
}

const fn entry(r: u8, g: u8, b: u8, idx: u8, ansi: Color) -> Entry {
    Entry {
        rgb: (r, g, b),
        idx,
        ansi,
    }
}

/// A theme, resolved. Fixed-size and `Copy` — see the module doc for why that
/// is a constraint rather than a preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Indexed by [`slot`], so the array order is the `Role` declaration order
    /// and nothing else may assume it.
    roles: [Entry; 8],
    /// `[foreground, background]`. Both halves are always present, which is the
    /// whole reason a background is expressible at all.
    chip: [Entry; 2],
    selection: [Entry; 2],
}

/// Where a role sits in [`Theme::roles`].
const fn slot(role: Role) -> usize {
    match role {
        Role::Text => 0,
        Role::Dim => 1,
        Role::Ok => 2,
        Role::Err => 3,
        Role::Warn => 4,
        Role::Info => 5,
        Role::Accent => 6,
        Role::Ground => 7,
    }
}

/// Today's palette, verbatim, and the thing every other theme falls back to.
///
/// It is `pub` because it is the answer to "what does Emma look like out of the
/// box", and it is pinned by a test against the live `Palette` at all three
/// fidelities: the day these two disagree, one of them is wrong about what the
/// product renders, and the whole safety of moving this data into a file is
/// that they cannot.
///
/// **`Err` and `Dim` declare indices the derivation would not produce.** 167
/// and 245 are the customary Gruvbox terminal mapping; nearest-distance says
/// 203 and 246. Both are the same hue and neither is wrong, so the divergence
/// is documented rather than absorbed — the shipping palette does not change
/// under a refactor that was supposed to move it, not alter it.
pub const BUILTIN: Theme = Theme {
    roles: [
        // Never reached through `Palette::color`, which short-circuits `Text`
        // to `Color::Reset` before a theme is consulted. Present so the array
        // is total, and so an edit reaching for a foreground hex has to walk
        // past the reason.
        entry(235, 219, 178, 223, Color::Reset),
        entry(143, 143, 148, 245, Color::DarkGray),
        entry(184, 187, 38, 142, Color::LightGreen),
        entry(251, 73, 52, 167, Color::LightRed),
        entry(250, 189, 47, 214, Color::LightYellow),
        entry(142, 192, 124, 108, Color::LightCyan),
        entry(245, 84, 143, 204, Color::LightMagenta),
        entry(13, 13, 16, 233, Color::Black),
    ],
    // Dark on the accent: the answer keys, chosen for contrast against the pink
    // rather than for resemblance to anything.
    chip: [
        entry(13, 13, 16, 233, Color::Black),
        entry(245, 84, 143, 204, Color::LightMagenta),
    ],
    // Accent text on a barely-raised near-black, measured off
    // the approved TUI mockup: rgb(25,27,30) over the mockup's own ground. Index
    // 234 is the greyscale ramp's `#1c1c1c`; `DarkGray` is as subtle as sixteen
    // colours get.
    selection: [
        entry(245, 84, 143, 204, Color::LightMagenta),
        entry(25, 27, 30, 234, Color::DarkGray),
    ],
};

impl Theme {
    /// The 24-bit value. `Role::Text` answers with the built-in's, which no
    /// caller should be asking for — see [`BUILTIN`].
    pub fn rgb(&self, role: Role) -> (u8, u8, u8) {
        self.roles[slot(role)].rgb
    }

    /// The xterm cube index, declared or derived.
    pub fn indexed(&self, role: Role) -> u8 {
        self.roles[slot(role)].idx
    }

    /// The named ANSI colour, declared or inherited — never derived. See the
    /// module doc.
    pub fn ansi16(&self, role: Role) -> Color {
        self.roles[slot(role)].ansi
    }

    /// `(foreground, background)`. Both halves always move together, which is
    /// the invariant that makes a background safe to express at all.
    pub fn pair(&self, pair: Pair) -> ((u8, u8, u8), (u8, u8, u8)) {
        let halves = self.halves(pair);
        (halves[0].rgb, halves[1].rgb)
    }

    fn halves(&self, pair: Pair) -> &[Entry; 2] {
        match pair {
            Pair::Chip => &self.chip,
            Pair::Selection => &self.selection,
        }
    }

    fn halves_mut(&mut self, pair: Pair) -> &mut [Entry; 2] {
        match pair {
            Pair::Chip => &mut self.chip,
            Pair::Selection => &mut self.selection,
        }
    }
}

// endregion: The resolved theme

// region: Deriving the index
// ---------------------------------------------------------------------------
// Deriving the index
//
// The one piece of arithmetic in this file, and the only measured claim in the
// design: nearest neighbour by squared RGB distance over the 6x6x6 cube and the
// 24-step greyscale ramp reproduces five of the seven indices `palette.rs`
// chose by convention. It is mechanical, it is right where it matters, and
// where it is wrong it is wrong by a shade of the same hue.
// ---------------------------------------------------------------------------

/// The six levels the xterm cube is built from. Not evenly spaced, which is why
/// this is a table rather than a multiplication.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The nearest xterm index to a colour, over indices 16..=255.
///
/// The first sixteen are deliberately excluded: they are whatever the user's
/// terminal theme says they are, so "nearest" is a claim about a colour this
/// process cannot see. The cube and the ramp are fixed by the specification and
/// are therefore the only two things here worth measuring against.
///
/// `pub(super)` because `palette.rs` needs the same arithmetic for a *pair*,
/// whose halves carry a hex and have no role to inherit an index from. Two
/// copies of one measured table is one more than the design's §2.3 has receipts
/// for, so this is the one that is meant to survive.
pub(super) fn derive_index((r, g, b): (u8, u8, u8)) -> u8 {
    let dist = |(x, y, z): (u8, u8, u8)| {
        let d = |a: u8, c: u8| {
            let d = i32::from(a) - i32::from(c);
            d * d
        };
        d(x, r) + d(y, g) + d(z, b)
    };
    let mut best = 16u8;
    let mut best_d = i32::MAX;
    for (ri, &rv) in CUBE.iter().enumerate() {
        for (gi, &gv) in CUBE.iter().enumerate() {
            for (bi, &bv) in CUBE.iter().enumerate() {
                let d = dist((rv, gv, bv));
                if d < best_d {
                    best_d = d;
                    best = 16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8;
                }
            }
        }
    }
    for i in 0..24u8 {
        let v = 8 + 10 * i;
        let d = dist((v, v, v));
        if d < best_d {
            best_d = d;
            best = 232 + i;
        }
    }
    best
}

// endregion: Deriving the index

// region: Reading a theme file
// ---------------------------------------------------------------------------
// Reading a theme file
//
// The whole of this section is about failing usefully. Nothing here returns a
// `Result`, on purpose: the failure policy is partial application, and a `?`
// anywhere in it would throw away the seven roles that parsed because the
// eighth did not.
// ---------------------------------------------------------------------------

/// Names a theme file may not claim. `emma` is the compiled default, and a file
/// of that name in a cloned repository would otherwise be the one way a
/// directory could silently change what stock Emma looks like.
const RESERVED: [&str; 1] = ["emma"];

/// The compiled-in theme's name — what an absent `settings.json` key means, and
/// the one name a file may not claim. `session_command`'s `/theme` calls the
/// same string `BUILT_IN`; they must stay the same word.
pub const BUILT_IN: &str = RESERVED[0];

/// Every role name a file may use, including the one that is refused — a typo
/// of `text` should be told about `text`, not about seven names that do not
/// include it.
const ROLE_NAMES: [&str; 8] = [
    "text", "dim", "ok", "err", "warn", "info", "accent", "ground",
];

/// The settable roles, by the name a file spells them with.
const SETTABLE: [(&str, Role); 7] = [
    ("dim", Role::Dim),
    ("ok", Role::Ok),
    ("err", Role::Err),
    ("warn", Role::Warn),
    ("info", Role::Info),
    ("accent", Role::Accent),
    ("ground", Role::Ground),
];

/// How many problems are worth putting above somebody's first prompt. A wall of
/// warnings is its own defect; past this the count and the path are what the
/// user needs, because the file is where they are going to go and look.
const MAX_NOTICES: usize = 5;

/// The theme in force, and everything that went wrong getting there.
///
/// **This never fails and never blocks the boot.** A missing file, a file that
/// is not JSON, a hex that is not a hex — each costs exactly as much colour as
/// it has to and produces a sentence saying so. The caller's job is to put
/// those sentences on screen through the same channel every other Emma
/// observation uses; at `Level::None` they are the only honest thing to say,
/// because the theme was never going to be consulted anyway.
///
/// `name` is the one-run override. With `None` the selection comes from
/// `~/.emma/settings.json`, and never from the project's `config.json`:
/// `settings.rs` already ruled that a personal default in a project's
/// configuration is two files answering one question with no rule for
/// disagreeing.
///
/// **User themes beat project themes on a name collision.** A project cannot
/// *select* a theme, so shadowing a name somebody already chose is the only way
/// a repository could repaint their screen, and this order is the door that
/// closes. The reverse — your own theme winning over the project's — is not a
/// risk; it is the preference working.
pub fn load(
    home: Option<&Path>,
    harness_root: Option<&Path>,
    name: Option<&str>,
) -> (Theme, Vec<String>) {
    let mut notices = Vec::new();
    let selected = match name {
        Some(n) => Some(n.to_string()),
        None => home.and_then(|h| crate::settings::load(h).theme),
    };
    let Some(selected) = selected else {
        return (BUILTIN, notices);
    };

    let searched: Vec<PathBuf> = [
        home.map(|h| h.join(".emma").join("themes")),
        harness_root.map(|r| r.join("themes")),
    ]
    .into_iter()
    .flatten()
    .map(|dir| dir.join(format!("{selected}.json")))
    .collect();

    if RESERVED.contains(&selected.as_str()) {
        // The name resolves to the built-in either way; the notice exists so an
        // author who wrote a file of that name is told why it did nothing,
        // rather than concluding their JSON is broken.
        if let Some(path) = searched.iter().find(|p| p.exists()) {
            notices.push(format!(
                "{} is a built-in theme name, so {} is ignored; copy it to another name and select that",
                selected,
                path.display()
            ));
        }
        return (BUILTIN, notices);
    }

    let Some((path, raw)) = searched
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok().map(|raw| (p, raw)))
    else {
        notices.push(format!(
            "theme \"{selected}\" was not found; looked in {}; using the built-in",
            searched
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" and ")
        ));
        return (BUILTIN, notices);
    };

    let doc = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(serde_json::Value::Object(map)) => map,
        Ok(_) => {
            notices.push(format!(
                "{} is not a JSON object; using the built-in theme",
                path.display()
            ));
            return (BUILTIN, notices);
        }
        Err(e) => {
            // serde's line and column, verbatim: "expected `,` at line 7
            // column 3" is the whole of what the user needs, and paraphrasing
            // it would lose the position.
            notices.push(format!(
                "{} could not be read ({e}); using the built-in theme",
                path.display()
            ));
            return (BUILTIN, notices);
        }
    };

    let theme = apply(&doc, &selected, path, &mut notices);
    (theme, cap(notices, path))
}

/// Every theme name this run can select, built-in first, each directory
/// sorted, no duplicates.
///
/// **The Settings screen's Theme row cycles this.** The tree this row arrived
/// from kept a compiled-in `THEMES` table of `&'static str`, so the row could
/// step an array; here a theme is a *file*, so the list is a `Vec<String>` read
/// at the moment the screen opens.
///
/// The two rules that decide the order are [`load`]'s, restated because a
/// cycler that offered a name `load` will not resolve would be a control that
/// lies: the built-in is first and cannot be shadowed, and yours beats the
/// project's on a collision. `session_command`'s `/theme` listing answers a
/// wider question — it shows the shadowed files and says why each is ignored —
/// and these two must not disagree about which names are *live*.
pub fn names(home: Option<&Path>, harness_root: Option<&Path>) -> Vec<String> {
    let mut all = vec![RESERVED[0].to_string()];
    for dir in [
        home.map(|h| h.join(".emma").join("themes")),
        harness_root.map(|r| r.join("themes")),
    ]
    .into_iter()
    .flatten()
    {
        let mut here: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
            .collect();
        here.sort();
        for name in here {
            if !all.iter().any(|n| n == &name) {
                all.push(name);
            }
        }
    }
    all
}

/// Everything a file says, applied to the built-in one field at a time.
fn apply(
    doc: &serde_json::Map<String, serde_json::Value>,
    selected: &str,
    path: &Path,
    notices: &mut Vec<String>,
) -> Theme {
    let mut theme = BUILTIN;

    // Unknown keys are noted and ignored rather than refused, for the reason
    // `statusline.rs` gives for the same choice: this is a display preference
    // file, and a build that adds a key should not make the file unreadable to
    // the build beside it.
    for key in doc.keys() {
        match key.as_str() {
            "name" | "about" | "roles" | "pairs" => {}
            "background" | "ground_fill" => notices.push(format!(
                "{key} is not settable: the terminal's background is the terminal theme's, \
                 and Emma cannot ask what it currently is — backgrounds are declared as pairs, \
                 which set both halves"
            )),
            "glyphs" => notices.push(
                "glyphs is not settable: the ASCII set exists because a console at code page 437 \
                 renders box drawing as mojibake, which is detected rather than chosen — \
                 EMMA_ASCII_FRAME is the supported control"
                    .to_string(),
            ),
            other => notices.push(format!("{other} is not a theme key; ignored")),
        }
    }

    // The filename is the identity, so a `name` that disagrees with it is a
    // note and not an error: the file still works, and being told is what stops
    // somebody editing the wrong one for an hour.
    if let Some(claimed) = doc.get("name").and_then(serde_json::Value::as_str) {
        if claimed != selected {
            notices.push(format!(
                "{} calls itself \"{claimed}\"; the filename is the name, so it is \"{selected}\"",
                path.display()
            ));
        }
    }

    // Roles first: a pair may name one, and it must see the theme's value
    // rather than the built-in's.
    if let Some(roles) = doc.get("roles") {
        apply_roles(&mut theme, roles, notices);
    }
    if let Some(pairs) = doc.get("pairs") {
        apply_pairs(&mut theme, pairs, notices);
    }
    theme
}

fn apply_roles(theme: &mut Theme, roles: &serde_json::Value, notices: &mut Vec<String>) {
    let Some(roles) = roles.as_object() else {
        notices.push("roles must be an object of role names to colours; ignored".to_string());
        return;
    };
    for (name, spec) in roles {
        if name == "text" {
            notices.push(
                "roles.text is not settable: body text is the terminal's own foreground, and a \
                 foreground chosen against one background is invisible on the opposite one — \
                 which the person it breaks for reads as Emma being broken, not the theme"
                    .to_string(),
            );
            continue;
        }
        let Some(&(_, role)) = SETTABLE.iter().find(|(n, _)| n == name) else {
            notices.push(format!(
                "\"{name}\" is not a role; the roles are {}",
                ROLE_NAMES.join(", ")
            ));
            continue;
        };
        if let Some(entry) = role_entry(name, role, spec, notices) {
            theme.roles[slot(role)] = entry;
        }
    }
}

/// One role's three values, or `None` when the role keeps the built-in's.
///
/// The granularity is the point: a typo in one hex costs that role and leaves
/// the other seven applied, because a theme thrown away over one character is a
/// theme its author cannot debug.
fn role_entry(
    name: &str,
    role: Role,
    spec: &serde_json::Value,
    notices: &mut Vec<String>,
) -> Option<Entry> {
    let inherited = BUILTIN.roles[slot(role)];
    let (hex, declared_idx, declared_ansi) = match spec {
        serde_json::Value::String(hex) => (hex.clone(), None, None),
        serde_json::Value::Object(map) => {
            for key in map.keys() {
                if !matches!(key.as_str(), "hex" | "ansi256" | "ansi16") {
                    notices.push(format!("roles.{name}.{key} is not a colour field; ignored"));
                }
            }
            let Some(hex) = map.get("hex").and_then(serde_json::Value::as_str) else {
                notices.push(format!(
                    "roles.{name} has no \"hex\"; there is nothing to derive the other \
                     fidelities from, so the built-in colour is kept"
                ));
                return None;
            };

            let idx = match map.get("ansi256") {
                None => None,
                Some(v) => match v.as_u64().filter(|n| *n <= 255) {
                    Some(n) => Some(n as u8),
                    None => {
                        notices.push(format!(
                            "roles.{name}.ansi256 must be a whole number from 0 to 255; \
                             the nearest cube entry is used instead"
                        ));
                        None
                    }
                },
            };
            let ansi = match map.get("ansi16") {
                None => None,
                Some(v) => match v.as_str().and_then(named_ansi) {
                    Some(c) => Some(c),
                    None => {
                        notices.push(format!(
                            "roles.{name}.ansi16 is not one of the sixteen colour names; \
                             the built-in role's name is kept"
                        ));
                        None
                    }
                },
            };
            (hex.to_string(), idx, ansi)
        }
        _ => {
            notices.push(format!(
                "roles.{name} must be a hex string or an object with a \"hex\"; \
                 the built-in colour is kept"
            ));
            return None;
        }
    };

    let Some(rgb) = parse_hex(&hex) else {
        notices.push(format!(
            "roles.{name}: \"{hex}\" is not a colour — write #rrggbb or #rgb; \
             the built-in colour is kept"
        ));
        return None;
    };
    Some(Entry {
        rgb,
        idx: declared_idx.unwrap_or_else(|| derive_index(rgb)),
        // Inherited, never derived. Distance at sixteen colours is exactly how
        // an accent becomes the colour that means failure.
        ansi: declared_ansi.unwrap_or(inherited.ansi),
    })
}

fn apply_pairs(theme: &mut Theme, pairs: &serde_json::Value, notices: &mut Vec<String>) {
    let Some(pairs) = pairs.as_object() else {
        notices.push("pairs must be an object of pair names to {fg, bg}; ignored".to_string());
        return;
    };
    for (name, spec) in pairs {
        let pair = match name.as_str() {
            "chip" => Pair::Chip,
            "selection" => Pair::Selection,
            other => {
                notices.push(format!(
                    "\"{other}\" is not a pair; the pairs are chip and selection"
                ));
                continue;
            }
        };
        if let Some(halves) = pair_halves(theme, name, pair, spec, notices) {
            *theme.halves_mut(pair) = halves;
        }
    }
}

/// Both halves of a pair, or `None` when the pair keeps the built-in's.
///
/// Two invariants, and neither is negotiable. **Both halves or neither**: a
/// half-declared pair applied half-way is a foreground against a background
/// nobody chose. And **`fg != bg` after resolution at every fidelity**, checked
/// on the resolved values rather than on the hexes, because the realistic
/// failure is two different hexes collapsing onto the same named colour at
/// sixteen — which a check on the hexes cannot see.
///
/// Not attempted: a contrast ratio. WCAG arithmetic against a terminal whose
/// gamma and palette are unknown is a number that looks authoritative and is
/// not, and this repository has a lesson about that exact shape of claim.
fn pair_halves(
    theme: &Theme,
    name: &str,
    pair: Pair,
    spec: &serde_json::Value,
    notices: &mut Vec<String>,
) -> Option<[Entry; 2]> {
    let built_in = *BUILTIN.halves(pair);
    let Some(map) = spec.as_object() else {
        notices.push(format!(
            "pairs.{name} must be an object with \"fg\" and \"bg\"; the built-in pair is kept"
        ));
        return None;
    };
    for key in map.keys() {
        if !matches!(key.as_str(), "fg" | "bg") {
            notices.push(format!(
                "pairs.{name}.{key} is not a half of a pair; ignored"
            ));
        }
    }
    let mut halves = [built_in[0]; 2];
    for (i, half) in ["fg", "bg"].iter().enumerate() {
        let Some(value) = map.get(*half).and_then(serde_json::Value::as_str) else {
            notices.push(format!(
                "pairs.{name} needs both \"fg\" and \"bg\" — a background without the \
                 foreground that has to be legible on it is the one thing a pair exists to \
                 prevent; the built-in pair is kept"
            ));
            return None;
        };
        halves[i] = match half_entry(theme, value, built_in[i]) {
            Some(entry) => entry,
            None => {
                notices.push(format!(
                    "pairs.{name}.{half}: \"{value}\" is neither a role nor a hex colour; \
                     the built-in pair is kept"
                ));
                return None;
            }
        };
    }
    if halves[0].rgb == halves[1].rgb
        || halves[0].idx == halves[1].idx
        || halves[0].ansi == halves[1].ansi
    {
        notices.push(format!(
            "pairs.{name} resolves to the same colour on both halves at one of the three \
             fidelities, which would draw it invisible; the built-in pair is kept"
        ));
        return None;
    }
    Some(halves)
}

/// One half of a pair: a role's resolved colour, or a hex of its own.
///
/// A bare hex has no role to inherit a 16-colour name from, so it takes the
/// built-in half's — which is the same rule as everywhere else here, applied to
/// the only other thing that has one.
fn half_entry(theme: &Theme, value: &str, inherited: Entry) -> Option<Entry> {
    if let Some(rgb) = parse_hex(value) {
        return Some(Entry {
            rgb,
            idx: derive_index(rgb),
            ansi: inherited.ansi,
        });
    }
    // `text` is not a colour Emma has — it is whatever the terminal is already
    // drawing in — so it cannot be one half of a pair either.
    let (_, role) = SETTABLE.iter().find(|(n, _)| *n == value)?;
    Some(theme.roles[slot(*role)])
}

/// `#rrggbb`, or `#rgb` as its shorthand. Nothing else: a bare word is a colour
/// name in some other file's vocabulary and would be a guess in this one.
fn parse_hex(text: &str) -> Option<(u8, u8, u8)> {
    let body = text.strip_prefix('#')?;
    if !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    match body.len() {
        3 => {
            let mut c = body.chars();
            let dup = |c: Option<char>| byte(&format!("{0}{0}", c?));
            Some((dup(c.next())?, dup(c.next())?, dup(c.next())?))
        }
        6 => Some((byte(&body[0..2])?, byte(&body[2..4])?, byte(&body[4..6])?)),
        _ => None,
    }
}

/// The sixteen names a theme may declare, as ratatui spells them. `grey` is
/// accepted beside `gray` because half the world writes it that way and a
/// spelling is not an intent.
fn named_ansi(name: &str) -> Option<Color> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "dark_gray" | "dark_grey" => Color::DarkGray,
        "light_red" => Color::LightRed,
        "light_green" => Color::LightGreen,
        "light_yellow" => Color::LightYellow,
        "light_blue" => Color::LightBlue,
        "light_magenta" => Color::LightMagenta,
        "light_cyan" => Color::LightCyan,
        "white" => Color::White,
        _ => return None,
    })
}

/// Trim the notices to something a person will read, and say what was left out.
///
/// The overflow line is ASCII on purpose. It can be printed on a console that
/// has not been moved to UTF-8 — the fallback path never is — and a message
/// about a broken file that is itself mojibake is a second problem stacked on
/// the first.
fn cap(mut notices: Vec<String>, path: &Path) -> Vec<String> {
    if notices.len() > MAX_NOTICES {
        let rest = notices.len() - MAX_NOTICES;
        notices.truncate(MAX_NOTICES);
        notices.push(format!("...and {rest} more problems in {}", path.display()));
    }
    notices
}

// endregion: Reading a theme file

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::palette::{Level, Palette};

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

    /// Write `<home>/.emma/themes/<name>.json` and hand back the home.
    fn user_theme(home: &Path, name: &str, body: &str) {
        let dir = home.join(".emma").join("themes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.json")), body).unwrap();
    }

    fn project_theme(root: &Path, name: &str, body: &str) {
        let dir = root.join("themes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.json")), body).unwrap();
    }

    /// Select `name` by writing it into `~/.emma/settings.json`, which is the
    /// only place a selection may live.
    fn select(home: &Path, name: &str) {
        let mut settings = crate::settings::load(home);
        settings.theme = Some(name.to_string());
        crate::settings::save(home, &settings).unwrap();
    }

    // -----------------------------------------------------------------------
    // The built-in is today's palette
    //
    // This is the whole safety of moving the table into a file, so it is
    // asserted twice — and the two halves are worth different amounts, which is
    // worth saying plainly. The **literals** are the receipt: they were
    // transcribed from `table()` as it stood before this change, so any drift in
    // `BUILTIN` turns them red. The comparison against the live `Palette` was
    // the stronger of the two while `palette.rs` still held its own copy of the
    // numbers; now that it reads `BUILTIN`, that half is close to a tautology
    // and is kept for the *shape* it pins — that every role resolves to `Rgb` at
    // 24 bits, `Indexed` at 256 and a named colour at 16, which is a fidelity
    // rule rather than a colour.
    // -----------------------------------------------------------------------

    #[test]
    fn the_builtin_theme_is_what_emma_renders_today() {
        // The literals. If `table()` changes and this does not, the day's work
        // is a palette in two places disagreeing about the product.
        assert_eq!(BUILTIN.rgb(Role::Dim), (143, 143, 148));
        assert_eq!(BUILTIN.rgb(Role::Ok), (184, 187, 38));
        assert_eq!(BUILTIN.rgb(Role::Err), (251, 73, 52));
        assert_eq!(BUILTIN.rgb(Role::Warn), (250, 189, 47));
        assert_eq!(BUILTIN.rgb(Role::Info), (142, 192, 124));
        assert_eq!(BUILTIN.rgb(Role::Accent), (245, 84, 143));
        assert_eq!(BUILTIN.rgb(Role::Ground), (13, 13, 16));
        assert_eq!(BUILTIN.indexed(Role::Err), 167);
        assert_eq!(BUILTIN.indexed(Role::Dim), 245);
        assert_eq!(BUILTIN.ansi16(Role::Accent), Color::LightMagenta);

        // …and the live palette, role by role and fidelity by fidelity.
        for role in ROLES {
            if role == Role::Text {
                // Not themeable, and short-circuited before a theme is read.
                continue;
            }
            let (r, g, b) = BUILTIN.rgb(role);
            assert_eq!(
                Palette::new(Level::Truecolor).color(role),
                Color::Rgb(r, g, b),
                "{role:?} at 24-bit"
            );
            assert_eq!(
                Palette::new(Level::Ansi256).color(role),
                Color::Indexed(BUILTIN.indexed(role)),
                "{role:?} at 256"
            );
            assert_eq!(
                Palette::new(Level::Ansi16).color(role),
                BUILTIN.ansi16(role),
                "{role:?} at 16"
            );
        }
    }

    #[test]
    fn the_builtin_pairs_are_the_two_backgrounds_on_screen_today() {
        // The chip: dark on the accent, both halves from the role table.
        assert_eq!(
            BUILTIN.pair(Pair::Chip),
            (BUILTIN.rgb(Role::Ground), BUILTIN.rgb(Role::Accent))
        );
        // The band: accent on the mockup's raised near-black, measured off the
        // image and previously a private table in `sidebar.rs`.
        assert_eq!(
            BUILTIN.pair(Pair::Selection),
            (BUILTIN.rgb(Role::Accent), (25, 27, 30))
        );
        assert_eq!(BUILTIN.selection[1].idx, 234);
        assert_eq!(BUILTIN.selection[1].ansi, Color::DarkGray);
    }

    // -----------------------------------------------------------------------
    // Derivation
    // -----------------------------------------------------------------------

    /// The measured claim from the design, re-run here so it is a receipt
    /// rather than a paragraph: five of the seven current indices come out of
    /// the arithmetic exactly, and the two that do not are the two the built-in
    /// declares.
    #[test]
    fn derivation_reproduces_five_of_the_seven_shipped_indices() {
        assert_eq!(derive_index(BUILTIN.rgb(Role::Accent)), 204);
        assert_eq!(derive_index(BUILTIN.rgb(Role::Ok)), 142);
        assert_eq!(derive_index(BUILTIN.rgb(Role::Warn)), 214);
        assert_eq!(derive_index(BUILTIN.rgb(Role::Info)), 108);
        assert_eq!(derive_index(BUILTIN.rgb(Role::Ground)), 233);
        // And the two that diverge, pinned as divergences: the built-in
        // declares 167 and 245 because Gruvbox's terminal mapping does.
        assert_eq!(derive_index(BUILTIN.rgb(Role::Err)), 203);
        assert_eq!(derive_index(BUILTIN.rgb(Role::Dim)), 246);
        // The ends of both ranges, which is where an off-by-one in the ramp
        // would show.
        assert_eq!(derive_index((0, 0, 0)), 16);
        assert_eq!(derive_index((255, 255, 255)), 231);
        assert_eq!(derive_index((28, 28, 28)), 234);
    }

    #[test]
    fn a_hex_is_read_in_both_spellings_and_nothing_else_is_a_colour() {
        assert_eq!(parse_hex("#f5548f"), Some((245, 84, 143)));
        assert_eq!(parse_hex("#FFF"), Some((255, 255, 255)));
        assert_eq!(parse_hex("#0d0d10"), Some((13, 13, 16)));
        for bad in ["blue", "#12345", "#gggggg", "f5548f", "#", "#1234567"] {
            assert_eq!(parse_hex(bad), None, "{bad} was accepted as a colour");
        }
    }

    // -----------------------------------------------------------------------
    // Loading
    // -----------------------------------------------------------------------

    #[test]
    fn no_selection_and_no_files_is_the_builtin_and_says_nothing() {
        let home = tempfile::tempdir().unwrap();
        let (theme, notices) = load(Some(home.path()), None, None);
        assert_eq!(theme, BUILTIN);
        assert!(notices.is_empty(), "{notices:?}");
        // …and with nothing at all to read from, which is `-p` and every test.
        assert_eq!(load(None, None, None), (BUILTIN, Vec::new()));
    }

    #[test]
    fn a_theme_selected_in_settings_is_read_and_applied() {
        let home = tempfile::tempdir().unwrap();
        user_theme(
            home.path(),
            "oxide",
            r##"{ "name": "oxide", "about": "warmer", "roles": {
                   "accent": "#00aaff",
                   "err": { "hex": "#fb4934", "ansi256": 167, "ansi16": "red" } } }"##,
        );
        select(home.path(), "oxide");
        let (theme, notices) = load(Some(home.path()), None, None);
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(theme.rgb(Role::Accent), (0, 170, 255));
        // Derived, not declared.
        assert_eq!(theme.indexed(Role::Accent), derive_index((0, 170, 255)));
        // Inherited, not derived: the file said nothing about sixteen colours.
        assert_eq!(theme.ansi16(Role::Accent), Color::LightMagenta);
        // Declared beats both.
        assert_eq!(theme.indexed(Role::Err), 167);
        assert_eq!(theme.ansi16(Role::Err), Color::Red);
        // Everything unmentioned is the built-in's, not black.
        assert_eq!(theme.rgb(Role::Ok), BUILTIN.rgb(Role::Ok));
    }

    #[test]
    fn an_empty_object_is_a_valid_theme_identical_to_the_default() {
        // The property that makes "copy the default and change one line" work.
        let home = tempfile::tempdir().unwrap();
        user_theme(home.path(), "mine", "{}");
        let (theme, notices) = load(Some(home.path()), None, Some("mine"));
        assert_eq!(theme, BUILTIN);
        assert!(notices.is_empty(), "{notices:?}");
    }

    /// The rule that keeps Emma legible on a background nobody here can see.
    #[test]
    fn a_theme_cannot_colour_ordinary_text() {
        let home = tempfile::tempdir().unwrap();
        user_theme(
            home.path(),
            "loud",
            r##"{ "roles": { "text": "#ff00ff", "ok": "#00ff00" } }"##,
        );
        let (theme, notices) = load(Some(home.path()), None, Some("loud"));
        assert_eq!(
            theme.rgb(Role::Text),
            BUILTIN.rgb(Role::Text),
            "a theme set the user's own foreground"
        );
        assert!(
            notices.iter().any(|n| n.contains("roles.text")),
            "the refusal was silent: {notices:?}"
        );
        // …and the rest of the file still applied, which is what makes the
        // refusal a message rather than an obstacle.
        assert_eq!(theme.rgb(Role::Ok), (0, 255, 0));
    }

    /// Per-role granularity: the failure this prevents is a theme thrown away
    /// over one character, which its author cannot debug.
    #[test]
    fn a_bad_hex_costs_one_role_and_not_the_file() {
        let home = tempfile::tempdir().unwrap();
        user_theme(
            home.path(),
            "typo",
            r##"{ "roles": { "ok": "green", "warn": "#fabd2f" } }"##,
        );
        let (theme, notices) = load(Some(home.path()), None, Some("typo"));
        assert_eq!(theme.rgb(Role::Ok), BUILTIN.rgb(Role::Ok));
        assert_eq!(theme.rgb(Role::Warn), (250, 189, 47));
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("green"), "{notices:?}");
    }

    #[test]
    fn a_misspelled_role_is_told_what_the_roles_are() {
        let home = tempfile::tempdir().unwrap();
        user_theme(home.path(), "t", r##"{ "roles": { "acent": "#ffffff" } }"##);
        let (theme, notices) = load(Some(home.path()), None, Some("t"));
        assert_eq!(theme, BUILTIN);
        let msg = notices.join(" ");
        assert!(msg.contains("acent"), "{msg}");
        for role in ROLE_NAMES {
            assert!(msg.contains(role), "{role} was not offered: {msg}");
        }
    }

    #[test]
    fn an_out_of_range_index_falls_back_to_derivation_rather_than_to_the_role() {
        let home = tempfile::tempdir().unwrap();
        user_theme(
            home.path(),
            "t",
            r##"{ "roles": { "ok": { "hex": "#00aaff", "ansi256": 900, "ansi16": "puce" } } }"##,
        );
        let (theme, notices) = load(Some(home.path()), None, Some("t"));
        // The hex survived both bad fields.
        assert_eq!(theme.rgb(Role::Ok), (0, 170, 255));
        assert_eq!(theme.indexed(Role::Ok), derive_index((0, 170, 255)));
        assert_eq!(theme.ansi16(Role::Ok), BUILTIN.ansi16(Role::Ok));
        assert_eq!(notices.len(), 2, "{notices:?}");
    }

    /// A decorative subsystem that stops the program starting is the wrong
    /// trade, and this is the test that says so.
    #[test]
    fn a_file_that_is_not_json_still_boots_emma_and_names_itself() {
        let home = tempfile::tempdir().unwrap();
        user_theme(home.path(), "broken", "{ not json at all");
        let (theme, notices) = load(Some(home.path()), None, Some("broken"));
        assert_eq!(theme, BUILTIN);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("broken.json"), "{notices:?}");
        // serde's position, passed through rather than paraphrased.
        assert!(notices[0].contains("line"), "{notices:?}");

        // A JSON document that is not an object is the same answer.
        user_theme(home.path(), "list", "[1, 2, 3]");
        let (theme, notices) = load(Some(home.path()), None, Some("list"));
        assert_eq!(theme, BUILTIN);
        assert!(notices[0].contains("not a JSON object"), "{notices:?}");
    }

    #[test]
    fn a_theme_that_is_not_there_names_both_places_it_looked() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let (theme, notices) = load(Some(home.path()), Some(root.path()), Some("ghost"));
        assert_eq!(theme, BUILTIN);
        let msg = notices.join(" ");
        assert!(msg.contains("ghost"), "{msg}");
        assert!(msg.contains(".emma"), "{msg}");
        assert!(
            msg.contains(&root.path().join("themes").display().to_string()),
            "{msg}"
        );
    }

    /// The one door a repository could otherwise use to repaint somebody's
    /// screen: shadowing a name they already chose.
    #[test]
    fn a_project_theme_never_shadows_a_user_theme_of_the_same_name() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        user_theme(
            home.path(),
            "house",
            r##"{ "roles": { "ok": "#010101" } }"##,
        );
        project_theme(
            root.path(),
            "house",
            r##"{ "roles": { "ok": "#fefefe" } }"##,
        );
        let (theme, _) = load(Some(home.path()), Some(root.path()), Some("house"));
        assert_eq!(theme.rgb(Role::Ok), (1, 1, 1), "the project won");

        // …and with no user theme of that name, the project's is used: the
        // order is a precedence, not a ban.
        let empty = tempfile::tempdir().unwrap();
        let (theme, _) = load(Some(empty.path()), Some(root.path()), Some("house"));
        assert_eq!(theme.rgb(Role::Ok), (254, 254, 254));
    }

    #[test]
    fn a_file_named_after_a_builtin_is_ignored_and_says_why() {
        let home = tempfile::tempdir().unwrap();
        user_theme(home.path(), "emma", r##"{ "roles": { "ok": "#010101" } }"##);
        let (theme, notices) = load(Some(home.path()), None, Some("emma"));
        assert_eq!(theme, BUILTIN, "a file replaced the built-in theme");
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("built-in"), "{notices:?}");

        // Selecting the built-in without such a file is not a problem to
        // report — it is what the name means.
        let clean = tempfile::tempdir().unwrap();
        assert_eq!(load(Some(clean.path()), None, Some("emma")).1.len(), 0);
    }

    #[test]
    fn a_one_run_override_beats_the_saved_selection() {
        let home = tempfile::tempdir().unwrap();
        user_theme(home.path(), "a", r##"{ "roles": { "ok": "#010101" } }"##);
        user_theme(home.path(), "b", r##"{ "roles": { "ok": "#020202" } }"##);
        select(home.path(), "a");
        assert_eq!(
            load(Some(home.path()), None, Some("b")).0.rgb(Role::Ok),
            (2, 2, 2)
        );
        assert_eq!(
            load(Some(home.path()), None, None).0.rgb(Role::Ok),
            (1, 1, 1)
        );
    }

    // -----------------------------------------------------------------------
    // Pairs
    // -----------------------------------------------------------------------

    #[test]
    fn a_pair_may_name_roles_or_hexes_and_takes_the_themes_own_role_values() {
        let home = tempfile::tempdir().unwrap();
        user_theme(
            home.path(),
            "t",
            r##"{ "roles": { "accent": "#00aaff" },
                 "pairs": { "selection": { "fg": "accent", "bg": "#191b1e" } } }"##,
        );
        let (theme, notices) = load(Some(home.path()), None, Some("t"));
        assert!(notices.is_empty(), "{notices:?}");
        // The *theme's* accent, not the built-in's — roles are applied first
        // for exactly this reason.
        assert_eq!(theme.pair(Pair::Selection), ((0, 170, 255), (25, 27, 30)));
        assert_eq!(theme.selection[1].idx, derive_index((25, 27, 30)));
        // A bare hex has no role to inherit a name from, so it takes the
        // built-in half's.
        assert_eq!(theme.selection[1].ansi, BUILTIN.selection[1].ansi);
    }

    #[test]
    fn half_a_pair_is_never_applied_half_way() {
        let home = tempfile::tempdir().unwrap();
        user_theme(
            home.path(),
            "t",
            r##"{ "pairs": { "chip": { "bg": "#00aaff" } } }"##,
        );
        let (theme, notices) = load(Some(home.path()), None, Some("t"));
        assert_eq!(theme.pair(Pair::Chip), BUILTIN.pair(Pair::Chip));
        assert_eq!(notices.len(), 1, "{notices:?}");
    }

    /// Two different hexes can collapse onto one named colour at sixteen, which
    /// is why the check runs on the resolved values and not on the strings.
    #[test]
    fn a_pair_that_resolves_to_one_colour_at_any_fidelity_falls_back() {
        let home = tempfile::tempdir().unwrap();
        // Same colour outright.
        user_theme(
            home.path(),
            "same",
            r##"{ "pairs": { "chip": { "fg": "accent", "bg": "accent" } } }"##,
        );
        let (theme, notices) = load(Some(home.path()), None, Some("same"));
        assert_eq!(theme.pair(Pair::Chip), BUILTIN.pair(Pair::Chip));
        assert!(notices[0].contains("invisible"), "{notices:?}");

        // Two hexes that stay distinct at 24 bits and at 256, and collapse onto
        // one named colour at sixteen. This is the realistic failure and the
        // reason the check runs after resolution: on the strings alone the
        // chip below looks perfectly legible.
        user_theme(
            home.path(),
            "collapse",
            r##"{ "roles": { "ground": { "hex": "#0d0d10", "ansi16": "light_magenta" } },
                 "pairs": { "chip": { "fg": "ground", "bg": "accent" } } }"##,
        );
        let (theme, notices) = load(Some(home.path()), None, Some("collapse"));
        assert_eq!(
            theme.pair(Pair::Chip),
            BUILTIN.pair(Pair::Chip),
            "a chip that is one colour at sixteen was applied"
        );
        assert!(
            notices.iter().any(|n| n.contains("invisible")),
            "{notices:?}"
        );
    }

    #[test]
    fn a_pair_nobody_named_is_refused_by_name() {
        let home = tempfile::tempdir().unwrap();
        user_theme(
            home.path(),
            "t",
            r##"{ "pairs": { "banner": { "fg": "ok", "bg": "err" } } }"##,
        );
        let (theme, notices) = load(Some(home.path()), None, Some("t"));
        assert_eq!(theme, BUILTIN);
        assert!(notices[0].contains("banner"), "{notices:?}");
        assert!(notices[0].contains("selection"), "{notices:?}");
    }

    // -----------------------------------------------------------------------
    // Refused keys and the cap
    // -----------------------------------------------------------------------

    #[test]
    fn the_refused_keys_carry_their_reason_rather_than_unknown_field() {
        let home = tempfile::tempdir().unwrap();
        user_theme(
            home.path(),
            "t",
            r##"{ "background": "#000000", "glyphs": { "ok": "y" }, "sparkle": true }"##,
        );
        let (theme, notices) = load(Some(home.path()), None, Some("t"));
        assert_eq!(theme, BUILTIN);
        let msg = notices.join(" | ");
        assert!(msg.contains("terminal theme's"), "{msg}");
        assert!(msg.contains("437"), "{msg}");
        // …and an unknown decorative key is noted and ignored, never fatal: a
        // file written by a later build stays readable to this one.
        assert!(msg.contains("sparkle"), "{msg}");
    }

    #[test]
    fn a_wrecked_file_cannot_fill_the_screen_with_warnings() {
        let home = tempfile::tempdir().unwrap();
        user_theme(
            home.path(),
            "wreck",
            r##"{ "roles": { "ok": "a", "err": "b", "warn": "c", "info": "d",
                           "accent": "e", "dim": "f", "ground": "g" } }"##,
        );
        let (theme, notices) = load(Some(home.path()), None, Some("wreck"));
        assert_eq!(theme, BUILTIN, "seven bad hexes changed a colour");
        assert_eq!(notices.len(), MAX_NOTICES + 1, "{notices:?}");
        let last = notices.last().unwrap();
        assert!(last.contains("2 more problems"), "{last}");
        assert!(last.contains("wreck.json"), "{last}");
        assert!(
            last.is_ascii(),
            "an overflow line that cannot be printed on a legacy console"
        );
    }
}
