//! The rebindable chord layer: `~/.emma/keybindings.json`.
//!
//! **The help chord is `ctrl+/`, and a bare `?` on an empty box is not it.**
//! QUICK HELP advertises Ctrl+/ for help, so that is the chord this layer
//! binds and the one the sidebar prints. The `?` shortcut stays compiled in
//! `input::pane_key` beside `,` for Settings: it is a convenience on an empty
//! box rather than a chord, it cannot be typed by accident mid-sentence, and
//! it is not in this layer for the same reason `Enter` is not.
//!
//! **What is rebindable, and what is not.** The Alt tool chords (Terminal,
//! Code, Reveal, Search), the three pages (Settings, Memory, Harness) and
//! the two toggles (Sidebar, Help) are the layer a person may move. Alt+q and
//! Ctrl-C are not here at all: they are the way out of a running goal, and a
//! keymap file that could take the exit away is a keymap file that can lock
//! somebody in. That is a deliberate hole, and [`REBINDABLE`] is the whole
//! list rather than a filter applied somewhere else.
//!
//! **The file is read once, at startup, and the preset is live.** Those are
//! two different facts and the Settings rows say both. Reading once is the
//! LSP card's rule and for the same reason: nothing here watches the file, so
//! an edit in an editor applies to the next run. Selecting a preset is
//! different: every preset in the file was already parsed by that one read,
//! so switching between them swaps a table this process is already holding
//! and binds immediately. The alternative, making a preset change wait for a
//! restart as well, would be a control that does nothing on a screen whose
//! whole point is that its controls do something.
//!
//! **A duplicate chord is refused, not resolved.** Two actions on one chord
//! has no correct answer: whichever the loader picked would be arbitrary and
//! silent. Both clashing entries fall back to their compiled defaults and
//! [`Keymap::notes`] carries a line naming the chord and the actions that
//! wanted it, which the Settings row prints.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use serde::Deserialize;

// region: The vocabulary

/// One rebindable thing a chord can do.
///
/// Named for the surface rather than the key, so a rebound chord still reads
/// as what it opens. The names here are exactly the keys the file uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Settings,
    Memory,
    Harness,
    LaunchTerminal,
    Code,
    LaunchReveal,
    LaunchSearch,
    Sidebar,
    Help,
}

impl Action {
    /// The name this action wears in keybindings.json.
    pub fn name(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Memory => "memory",
            Self::Harness => "harness",
            Self::LaunchTerminal => "shell",
            Self::Code => "code",
            Self::LaunchReveal => "reveal",
            Self::LaunchSearch => "search",
            Self::Sidebar => "sidebar",
            Self::Help => "help",
        }
    }

    /// The action named exactly `name`, or `None`. An unknown name in the
    /// file is kept as a note rather than an error, the `lsp.enabled` rule.
    pub fn from_name(name: &str) -> Option<Self> {
        REBINDABLE.iter().copied().find(|a| a.name() == name)
    }
}

/// Every action a keybindings file may move, in the order the starter writes
/// them. The one list: nothing else decides what is rebindable.
pub const REBINDABLE: [Action; 9] = [
    Action::Settings,
    Action::Memory,
    Action::Harness,
    Action::LaunchTerminal,
    Action::Code,
    Action::LaunchReveal,
    Action::LaunchSearch,
    Action::Sidebar,
    Action::Help,
];

/// One chord: a printable key with the modifiers that reach it.
///
/// A `char` and two flags rather than a crossterm `KeyEvent`, because this is
/// the shape the file can spell and the shape `input::pane_key` matches on.
/// Function keys and arrows are not rebindable and are not representable
/// here, which is the same statement twice on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Chord {
    pub alt: bool,
    pub ctrl: bool,
    /// Lower case. Case is not a modifier here: a terminal reports Shift+m as
    /// `M` with no ALT, and treating that as a distinct chord would make half
    /// the bindings unreachable on a keyboard with caps lock on.
    pub key: char,
}

impl Chord {
    /// How the file spells this chord: `alt+m`, `ctrl+b`, `?`.
    pub fn spell(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("ctrl+");
        }
        if self.alt {
            s.push_str("alt+");
        }
        s.push(self.key);
        s
    }
}

/// A chord as a person reads it: `alt+m` becomes `Alt+m`, `ctrl+/` becomes
/// `Ctrl+/`.
///
/// The file's spelling is lower case and the screen's is not, and both the
/// sidebar's QUICK HELP and the Help page print chords beside prose that has
/// always written `Alt+q`. One function so the two surfaces cannot disagree
/// about the capital.
///
/// The key itself keeps its case: `?` is `?`, and a rebind to `x` must not
/// read as `X`, which is a different keystroke.
pub fn display(chord: &str) -> String {
    chord
        .split('+')
        .map(|part| match part {
            "ctrl" => "Ctrl",
            "alt" => "Alt",
            other => other,
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Read one chord. `alt+m`, `Ctrl+Alt+M`, `?`. Whitespace and case are
/// forgiven; an unknown modifier or a multi-character key is not.
pub fn parse_chord(s: &str) -> Option<Chord> {
    let mut alt = false;
    let mut ctrl = false;
    let mut key = None;
    for part in s.trim().split('+') {
        let part = part.trim();
        if part.is_empty() {
            // `alt++` is a plus with a modifier, not a syntax error: the
            // split leaves an empty field before the real one.
            if key.is_none() {
                key = Some('+');
            }
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "alt" | "option" | "meta" => alt = true,
            "ctrl" | "control" => ctrl = true,
            other => {
                let mut chars = other.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                if key.is_some() {
                    return None;
                }
                key = Some(c);
            }
        }
    }
    key.map(|key| Chord { alt, ctrl, key })
}

/// The compiled map: what Emma does with no file at all.
///
/// The single source of the defaults. `input::pane_key`'s own arms match
/// these exactly, and `defaults_agree_with_the_compiled_arms` is the test
/// that keeps them from drifting.
pub const DEFAULTS: [(Action, &str); 9] = [
    (Action::Settings, "alt+,"),
    (Action::Memory, "alt+m"),
    (Action::Harness, "alt+h"),
    (Action::LaunchTerminal, "alt+s"),
    (Action::Code, "alt+c"),
    (Action::LaunchReveal, "alt+f"),
    (Action::LaunchSearch, "alt+/"),
    (Action::Sidebar, "ctrl+b"),
    (Action::Help, "ctrl+/"),
];

/// What an absent or unnamed preset resolves to.
pub const DEFAULT_PRESET: &str = "default";

// endregion: The vocabulary

// region: The file

/// `~/.emma/keybindings.json`, as serde reads it.
///
/// Every field optional, because a file somebody hand-edited down to one key
/// must still load. Unknown fields are kept by being ignored rather than
/// rejected: a file written by a newer Emma still starts this one.
#[derive(Debug, Default, Deserialize)]
struct Document {
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    bindings: std::collections::BTreeMap<String, String>,
    /// A `Value` rather than a map of `Preset`, so a `_comment` key can sit
    /// beside the presets. The whole file documents itself with `_comment`
    /// entries and a strict type here would make the presets block the one
    /// place that convention does not work.
    #[serde(default)]
    presets: std::collections::BTreeMap<String, serde_json::Value>,
}

/// One preset's bindings, or an empty map for anything that is not one (a
/// `_comment`, most often).
fn preset_bindings(v: &serde_json::Value) -> std::collections::BTreeMap<String, String> {
    v.get("bindings")
        .and_then(|b| serde_json::from_value(b.clone()).ok())
        .unwrap_or_default()
}

/// The chords in force, plus everything the load had to say about them.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    /// Chord to action, defaults already folded in.
    table: Vec<(Chord, Action)>,
    /// The preset that produced [`Self::table`].
    pub preset: String,
    /// Every preset name this file offers, `default` first.
    pub presets: Vec<String>,
    /// What the load wants said: a duplicate chord refused, a name this build
    /// does not know. Empty on a clean load, which is the common case.
    pub notes: Vec<String>,
    /// Whether any binding differs from the compiled map. The input layer
    /// asks, because a default map must leave the compiled arms alone.
    pub customised: bool,
    /// Every preset's raw bindings, kept so a preset can be selected without
    /// re-reading the file. This is what makes the preset row live.
    all: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

impl Keymap {
    /// The compiled map, with nothing read from disk.
    pub fn compiled() -> Self {
        Self {
            table: DEFAULTS
                .iter()
                .map(|(a, c)| (parse_chord(c).expect("a compiled default parses"), *a))
                .collect(),
            preset: DEFAULT_PRESET.to_string(),
            presets: vec![DEFAULT_PRESET.to_string()],
            notes: Vec::new(),
            customised: false,
            all: Default::default(),
        }
    }

    /// What this chord does, if anything.
    pub fn lookup(&self, alt: bool, ctrl: bool, key: char) -> Option<Action> {
        let key = key.to_ascii_lowercase();
        self.table
            .iter()
            .find(|(c, _)| c.alt == alt && c.ctrl == ctrl && c.key == key)
            .map(|(_, a)| *a)
    }

    /// Whether this chord is a compiled default that the active map has moved
    /// away from.
    ///
    /// The half of the consult that makes a rebind real. `input::pane_key`
    /// keeps its compiled arms, so without this a remapped action would fire
    /// on both its new chord and its old one, and "I rebound it" would mean
    /// "I added one".
    pub fn shadowed(&self, alt: bool, ctrl: bool, key: char) -> bool {
        if !self.customised {
            return false;
        }
        let key = key.to_ascii_lowercase();
        let is_default = DEFAULTS.iter().any(|(_, spell)| {
            matches!(parse_chord(spell), Some(c) if c.alt == alt && c.ctrl == ctrl && c.key == key)
        });
        is_default && self.lookup(alt, ctrl, key).is_none()
    }

    /// The chord bound to `action`, as the file would spell it. The quick-help
    /// sidebar prints this rather than a compiled string.
    pub fn chord_for(&self, action: Action) -> Option<String> {
        self.table
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(c, _)| c.spell())
    }

    /// Swap to another preset this file already offered, keeping the notes.
    ///
    /// `None` when the name is not one of [`Self::presets`], so a caller can
    /// say so rather than silently landing on the default.
    pub fn with_preset(&self, name: &str) -> Option<Self> {
        if name == DEFAULT_PRESET {
            let mut next = Keymap::compiled();
            next.presets = self.presets.clone();
            next.all = self.all.clone();
            return Some(next);
        }
        let bindings = self.all.get(name)?.clone();
        let (table, notes, customised) = fold(&bindings);
        Some(Self {
            table,
            preset: name.to_string(),
            presets: self.presets.clone(),
            notes,
            customised,
            all: self.all.clone(),
        })
    }
}

/// Fold one binding map over the compiled defaults.
///
/// Returns the table, the notes, and whether anything actually moved. The
/// three rules live here and nowhere else: an unknown action name is a note,
/// an unreadable chord is a note, and a chord two actions both want is a note
/// plus a fallback to the compiled default **for both of them**.
fn fold(
    bindings: &std::collections::BTreeMap<String, String>,
) -> (Vec<(Chord, Action)>, Vec<String>, bool) {
    let mut notes = Vec::new();
    // Parse first, so a collision is detected across the whole file rather
    // than against whatever happened to be inserted before it.
    let mut wanted: Vec<(Action, Chord)> = Vec::new();
    for (name, spell) in bindings {
        let Some(action) = Action::from_name(name) else {
            notes.push(format!(
                "keybindings.json names no action {name}; this build rebinds {}",
                REBINDABLE
                    .iter()
                    .map(|a| a.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            continue;
        };
        let Some(chord) = parse_chord(spell) else {
            notes.push(format!(
                "keybindings.json: {name} is set to {spell}, which is not a chord this build \
                 can read; {name} keeps its default"
            ));
            continue;
        };
        wanted.push((action, chord));
    }
    // The duplicate refusal. Every action sharing a chord with another loses
    // it: picking a winner would be arbitrary, and the person who wrote the
    // file is the only one who knows which they meant.
    let clashing: Vec<Action> = wanted
        .iter()
        .filter(|(a, c)| wanted.iter().any(|(b, d)| b != a && d == c))
        .map(|(a, _)| *a)
        .collect();
    for chord in clashing
        .iter()
        .filter_map(|a| wanted.iter().find(|(b, _)| b == a).map(|(_, c)| *c))
        .collect::<std::collections::BTreeSet<_>>()
    {
        let names: Vec<&str> = wanted
            .iter()
            .filter(|(_, c)| *c == chord)
            .map(|(a, _)| a.name())
            .collect();
        notes.push(format!(
            "keybindings.json binds {} to {}; a chord cannot do two things, so all of them keep \
             their defaults",
            names.join(" and "),
            chord.spell()
        ));
    }
    let mut table: Vec<(Chord, Action)> = Vec::new();
    let mut customised = false;
    for action in REBINDABLE {
        let chosen = wanted
            .iter()
            .find(|(a, _)| *a == action && !clashing.contains(a))
            .map(|(_, c)| *c);
        let default = DEFAULTS
            .iter()
            .find(|(a, _)| *a == action)
            .and_then(|(_, spell)| parse_chord(spell))
            .expect("every rebindable action has a compiled default");
        let chord = chosen.unwrap_or(default);
        if chord != default {
            customised = true;
        }
        table.push((chord, action));
    }
    (table, notes, customised)
}

/// Read a keybindings file's text. Pure: no filesystem, so a test names the
/// bytes it means.
///
/// Unparseable JSON is not an error that stops Emma. It is a note and the
/// compiled map, because a keymap file with a stray comma should not be the
/// reason a terminal will not start.
pub fn parse(raw: &str) -> Keymap {
    let doc: Document = match serde_json::from_str(raw) {
        Ok(d) => d,
        Err(e) => {
            let mut map = Keymap::compiled();
            map.notes.push(format!(
                "keybindings.json could not be read ({e}); the compiled keys are in force"
            ));
            return map;
        }
    };
    let mut all = std::collections::BTreeMap::new();
    all.insert(DEFAULT_PRESET.to_string(), doc.bindings.clone());
    for (name, value) in &doc.presets {
        // `default` is the compiled map by definition; a preset claiming the
        // name would make "default" mean two things. A `_comment` is not a
        // preset either, and neither is refused: they are simply not presets.
        if name == DEFAULT_PRESET || !value.is_object() {
            continue;
        }
        all.insert(name.clone(), preset_bindings(value));
    }
    let mut presets: Vec<String> = vec![DEFAULT_PRESET.to_string()];
    presets.extend(
        doc.presets
            .iter()
            .filter(|(n, v)| n.as_str() != DEFAULT_PRESET && v.is_object())
            .map(|(n, _)| n.clone()),
    );
    // The file's top-level `bindings` are the default preset's overlay: a
    // person who only wants two keys moved should not have to invent a preset
    // name to do it.
    let chosen = doc.preset.unwrap_or_else(|| DEFAULT_PRESET.to_string());
    let bindings = all.get(&chosen).cloned().unwrap_or_default();
    let (table, mut notes, customised) = fold(&bindings);
    if !presets.contains(&chosen) {
        notes.push(format!(
            "keybindings.json selects the preset {chosen}, which the file does not define; the \
             default keys are in force"
        ));
    }
    Keymap {
        table,
        preset: if presets.contains(&chosen) {
            chosen
        } else {
            DEFAULT_PRESET.to_string()
        },
        presets,
        notes,
        customised,
        all,
    }
}

/// Where the file lives.
pub fn path(home: &Path) -> PathBuf {
    home.join(".emma").join("keybindings.json")
}

/// Read the file under `home`, or the compiled map when there is none.
pub fn load(home: &Path) -> Keymap {
    match std::fs::read_to_string(path(home)) {
        Ok(raw) => parse(&raw),
        Err(_) => Keymap::compiled(),
    }
}

/// The self-documenting file written on the first `Open Key Presets`.
///
/// **JSON has no comments, so the documentation is data.** `_comment` keys
/// are the convention here: they survive a round trip through every JSON tool,
/// serde ignores them because [`Document`] does not name them, and a person
/// editing the file reads the schema in the file rather than in a README they
/// have to go and find.
pub fn starter() -> String {
    let rows: Vec<String> = DEFAULTS
        .iter()
        .map(|(a, c)| format!("    \"{}\": \"{}\"", a.name(), c))
        .collect();
    format!(
        r#"{{
  "_comment": "Emma keybindings. Read once at startup: an edit here applies to the next run.",
  "_comment_actions": "Rebindable actions: {actions}. Alt+q and Ctrl-C are not rebindable, because the way out of a running goal is not a preference.",
  "_comment_chords": "A chord is modifiers plus one printable key, joined by +: alt+m, ctrl+b, ctrl+alt+s, ?. Case does not matter.",
  "_comment_duplicates": "Two actions on one chord is refused: both fall back to their defaults and the Settings screen says which.",
  "_comment_preset": "preset names which entry of presets is active. default is the compiled map below and cannot be redefined.",
  "preset": "default",
  "bindings": {{
{rows}
  }},
  "presets": {{
    "_comment": "Add your own here, for example \"lefthand\": {{ \"bindings\": {{ \"settings\": \"ctrl+alt+,\" }} }}. Anything a preset leaves out keeps its default."
  }}
}}
"#,
        actions = REBINDABLE
            .iter()
            .map(|a| a.name())
            .collect::<Vec<_>>()
            .join(", "),
        rows = rows.join(",\n"),
    )
}

// endregion: The file

// region: The active map

/// The map every keystroke is resolved against.
///
/// A `RwLock` behind a `OnceLock` rather than a plain static: the file is read
/// once, and the preset within it may be swapped afterwards. Reads are on the
/// key path and take the read half, which is uncontended in practice because
/// the only writer is a Settings row somebody pressed.
fn cell() -> &'static RwLock<Arc<Keymap>> {
    static CELL: OnceLock<RwLock<Arc<Keymap>>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(Arc::new(Keymap::compiled())))
}

/// The map in force. Cheap: one `Arc` clone.
pub fn active() -> Arc<Keymap> {
    cell()
        .read()
        .map(|g| g.clone())
        .unwrap_or_else(|e| e.into_inner().clone())
}

/// The lock every test that installs a keymap takes first.
///
/// The keymap is one process-wide cell and the test harness runs tests on
/// parallel threads: `bindings.rs`, `help.rs` and this file all install into
/// it. Without this a green run is a scheduling accident.
#[doc(hidden)]
pub fn test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &LOCK
}

/// Install a map, from the startup read or from a preset switch.
pub fn install(map: Keymap) {
    let next = Arc::new(map);
    match cell().write() {
        Ok(mut g) => *g = next,
        Err(e) => *e.into_inner() = next,
    }
}

// endregion: The active map

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chord_reads_its_modifiers_and_its_key() {
        assert_eq!(
            parse_chord("alt+m"),
            Some(Chord {
                alt: true,
                ctrl: false,
                key: 'm'
            })
        );
        assert_eq!(
            parse_chord("Ctrl+Alt+S"),
            Some(Chord {
                alt: true,
                ctrl: true,
                key: 's'
            })
        );
        assert_eq!(
            parse_chord("?"),
            Some(Chord {
                alt: false,
                ctrl: false,
                key: '?'
            })
        );
        assert_eq!(
            parse_chord("alt+f5"),
            None,
            "a multi-character key is not a chord"
        );
        assert_eq!(parse_chord("alt"), None, "a modifier alone is not a chord");
    }

    /// The compiled defaults are the same chords `input::pane_key` matches on.
    /// Stated here as data so the two cannot drift apart quietly.
    #[test]
    fn every_default_parses_and_is_unique() {
        let chords: Vec<Chord> = DEFAULTS
            .iter()
            .map(|(_, c)| parse_chord(c).expect("a compiled default parses"))
            .collect();
        for (i, c) in chords.iter().enumerate() {
            assert!(
                !chords[i + 1..].contains(c),
                "two compiled defaults share {}",
                c.spell()
            );
        }
        assert_eq!(chords.len(), REBINDABLE.len());
    }

    #[test]
    fn a_remapped_chord_fires_and_the_default_no_longer_does() {
        let map = parse(r#"{ "bindings": { "memory": "alt+k" } }"#);
        assert!(
            map.notes.is_empty(),
            "a clean file has nothing to say: {:?}",
            map.notes
        );
        assert_eq!(map.lookup(true, false, 'k'), Some(Action::Memory));
        assert_eq!(map.lookup(true, false, 'm'), None, "the old chord is gone");
        assert!(
            map.shadowed(true, false, 'm'),
            "the compiled arm must be shadowed"
        );
        assert!(
            !map.shadowed(true, false, 'h'),
            "an untouched default still fires"
        );
    }

    #[test]
    fn the_compiled_map_shadows_nothing() {
        let map = Keymap::compiled();
        assert!(!map.shadowed(true, false, 'm'));
        assert_eq!(map.lookup(true, false, 'm'), Some(Action::Memory));
        assert_eq!(map.lookup(false, true, 'b'), Some(Action::Sidebar));
        // Help's compiled chord is Ctrl+/, the one QUICK HELP advertises.
        // A bare `?` on an empty box still opens help, and is a compiled
        // convenience in `input::pane_key` rather than a member of this
        // layer, so this map does not know it.
        assert_eq!(map.lookup(false, true, '/'), Some(Action::Help));
        assert_eq!(map.lookup(false, false, '?'), None);
    }

    #[test]
    fn a_duplicate_chord_is_refused_and_both_entries_fall_back() {
        let map = parse(r#"{ "bindings": { "memory": "alt+k", "harness": "alt+k" } }"#);
        assert_eq!(
            map.lookup(true, false, 'k'),
            None,
            "neither action takes the contested chord"
        );
        assert_eq!(map.lookup(true, false, 'm'), Some(Action::Memory));
        assert_eq!(map.lookup(true, false, 'h'), Some(Action::Harness));
        assert!(!map.customised, "nothing moved, so nothing is shadowed");
        let note = map.notes.join(" ");
        assert!(note.contains("alt+k"), "the note names the chord: {note}");
        assert!(
            note.contains("memory") && note.contains("harness"),
            "and both actions: {note}"
        );
    }

    #[test]
    fn an_unknown_action_name_is_a_note_and_not_an_error() {
        let map = parse(r#"{ "bindings": { "teleport": "alt+t" } }"#);
        assert_eq!(map.lookup(true, false, 'm'), Some(Action::Memory));
        assert!(map.notes.join(" ").contains("teleport"));
    }

    #[test]
    fn an_unreadable_chord_is_a_note_and_the_action_keeps_its_default() {
        let map = parse(r#"{ "bindings": { "memory": "hyper+m" } }"#);
        assert_eq!(map.lookup(true, false, 'm'), Some(Action::Memory));
        assert!(map.notes.join(" ").contains("memory"));
    }

    #[test]
    fn broken_json_leaves_the_compiled_keys_in_force() {
        let map = parse("{ not json");
        assert_eq!(map.lookup(true, false, 'm'), Some(Action::Memory));
        assert!(map.notes.join(" ").contains("could not be read"));
    }

    #[test]
    fn a_preset_is_selected_from_the_one_read_and_default_is_never_redefined() {
        let map = parse(
            r#"{ "preset": "lefthand",
                 "presets": { "lefthand": { "bindings": { "settings": "alt+;" } },
                              "default":  { "bindings": { "settings": "alt+x" } } } }"#,
        );
        assert_eq!(map.preset, "lefthand");
        assert_eq!(map.lookup(true, false, ';'), Some(Action::Settings));
        assert_eq!(map.presets, vec!["default", "lefthand"]);
        let back = map
            .with_preset("default")
            .expect("default is always offered");
        assert_eq!(back.lookup(true, false, ','), Some(Action::Settings));
        assert_eq!(
            back.lookup(true, false, 'x'),
            None,
            "a preset cannot redefine default"
        );
        assert_eq!(
            back.presets, map.presets,
            "the preset list survives the switch"
        );
    }

    #[test]
    fn a_preset_the_file_does_not_define_is_a_note_and_the_defaults_hold() {
        let map = parse(r#"{ "preset": "nope" }"#);
        assert_eq!(map.preset, DEFAULT_PRESET);
        assert!(map.notes.join(" ").contains("nope"));
    }

    #[test]
    fn the_starter_file_parses_as_the_compiled_map_and_documents_itself() {
        let text = starter();
        let map = parse(&text);
        assert!(
            map.notes.is_empty(),
            "the starter must load clean: {:?}",
            map.notes
        );
        assert!(!map.customised, "the starter spells the compiled map");
        for (action, spell) in DEFAULTS {
            assert_eq!(map.chord_for(action).as_deref(), Some(spell));
        }
        assert!(
            text.contains("\"_comment\""),
            "the schema documents itself in the file"
        );
        assert!(text.contains("next run"), "and says when an edit applies");
    }

    #[test]
    fn load_without_a_file_is_the_compiled_map() {
        let home = tempfile::tempdir().expect("temp home");
        let map = load(home.path());
        assert!(!map.customised);
        assert_eq!(map.lookup(true, false, 'm'), Some(Action::Memory));
    }

    #[test]
    fn load_reads_the_file_under_the_given_home() {
        let home = tempfile::tempdir().expect("temp home");
        let file = path(home.path());
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, r#"{ "bindings": { "harness": "alt+j" } }"#).expect("write");
        let map = load(home.path());
        assert_eq!(map.lookup(true, false, 'j'), Some(Action::Harness));
    }
}
