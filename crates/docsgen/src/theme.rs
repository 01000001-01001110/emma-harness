//! The palette every generated diagram draws with, and the reason it is one
//! file.
//!
//! The hand-drawn diagrams each chose their own hex values, so a reader
//! comparing two pages was comparing two conventions. With one palette, a
//! magenta edge carries the same meaning on every page.
//!
//! # Why the colours are written into the SVG
//!
//! `docs/` opens from a file with no server, and a page can be saved and read
//! on its own. A diagram depending on a class in `docs/assets/docs.css` would
//! render black-on-black once separated from that file.
//!
//! Each colour is emitted twice, in a `<style>` the SVG carries, switched by
//! `prefers-color-scheme`, so the generator never has to know the reader's
//! theme.
//!
//! The same seven values appear in `docs/assets/docs.css`. Nothing checks that
//! the two agree.

/// One colour, in its light and dark spellings.
pub struct Duo {
    pub light: &'static str,
    pub dark: &'static str,
}

/// The cyberpunk palette: near-black grounds, cyan structure, magenta emphasis.
///
/// [`Palette::ACCENT`] marks one edge or box per diagram. A diagram that
/// highlights six things highlights nothing; a second emphasis usually means
/// two diagrams.
pub struct Palette;

impl Palette {
    /// Behind the drawing. Dark in both themes: a cyberpunk diagram on white
    /// is a diagram with a different argument.
    pub const GROUND: Duo = Duo {
        light: "#12101a",
        dark: "#0b0a10",
    };
    /// Box fills.
    pub const SURFACE: Duo = Duo {
        light: "#1c1930",
        dark: "#15132a",
    };
    /// Box borders and ordinary edges.
    pub const LINE: Duo = Duo {
        light: "#31d4f2",
        dark: "#22b8d6",
    };
    /// Label text.
    pub const TEXT: Duo = Duo {
        light: "#dcf6ff",
        dark: "#c4e8f5",
    };
    /// Secondary text: annotations, edge labels, anything the reader should
    /// find only after the shape.
    pub const MUTED: Duo = Duo {
        light: "#7d8fa8",
        dark: "#6b7c94",
    };
    /// The one thing this diagram is about.
    pub const ACCENT: Duo = Duo {
        light: "#ff2e97",
        dark: "#ff45a3",
    };
    /// A relationship that is real but conditional -- a dev-dependency, a path
    /// taken only on one platform. Drawn dashed as well as recoloured, because
    /// colour alone is not a distinction every reader can see.
    pub const DIM: Duo = Duo {
        light: "#8a5cf0",
        dark: "#7a4fd8",
    };
}

/// The `<style>` block every generated SVG carries.
///
/// Emitted once per diagram rather than shared, for the reason in the module
/// doc: a page saved on its own must still render.
pub fn style(prefix: &str) -> String {
    let mut s = String::new();
    s.push_str("  <style>\n");
    s.push_str(&vars(prefix, false));
    s.push_str("    @media (prefers-color-scheme: dark) {\n");
    s.push_str(&vars(prefix, true));
    s.push_str("    }\n");
    s.push_str(&format!(
        "    .{p}-ground {{ fill: var(--{p}-ground); }}\n\
         \x20   .{p}-box {{ fill: var(--{p}-surface); stroke: var(--{p}-line); stroke-width: 1.5; }}\n\
         \x20   .{p}-box-hl {{ fill: var(--{p}-surface); stroke: var(--{p}-accent); stroke-width: 2; }}\n\
         \x20   .{p}-lbl {{ fill: var(--{p}-text); font: 600 13px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }}\n\
         \x20   .{p}-note {{ fill: var(--{p}-muted); font: 400 11px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }}\n\
         \x20   .{p}-edge {{ stroke: var(--{p}-line); stroke-width: 1.5; fill: none; }}\n\
         \x20   .{p}-edge-hl {{ stroke: var(--{p}-accent); stroke-width: 2; fill: none; }}\n\
         \x20   .{p}-edge-dim {{ stroke: var(--{p}-dim); stroke-width: 1.5; fill: none; stroke-dasharray: 5 4; }}\n",
        p = prefix
    ));
    s.push_str("  </style>\n");
    s
}

fn vars(prefix: &str, dark: bool) -> String {
    let pick = |d: &Duo| if dark { d.dark } else { d.light };
    let indent = if dark { "      " } else { "    " };
    format!(
        "{i}:root {{\n\
         {i}  --{p}-ground: {ground};\n\
         {i}  --{p}-surface: {surface};\n\
         {i}  --{p}-line: {line};\n\
         {i}  --{p}-text: {text};\n\
         {i}  --{p}-muted: {muted};\n\
         {i}  --{p}-accent: {accent};\n\
         {i}  --{p}-dim: {dim};\n\
         {i}}}\n",
        i = indent,
        p = prefix,
        ground = pick(&Palette::GROUND),
        surface = pick(&Palette::SURFACE),
        line = pick(&Palette::LINE),
        text = pick(&Palette::TEXT),
        muted = pick(&Palette::MUTED),
        accent = pick(&Palette::ACCENT),
        dim = pick(&Palette::DIM),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **If this breaks:** a diagram renders as one colour on itself in one of
    /// the two themes, and only a reader in that theme ever sees it.
    ///
    /// Catches a token added to the light block and forgotten in the dark
    /// one, which looks correct to whoever wrote it.
    #[test]
    fn every_token_is_defined_in_both_themes() {
        let s = style("t");
        let light = s.split("@media").next().expect("a light block");
        let dark = s.split("@media").nth(1).expect("a dark block");
        for token in [
            "ground", "surface", "line", "text", "muted", "accent", "dim",
        ] {
            let name = format!("--t-{token}:");
            assert!(light.contains(&name), "{token} is missing from light");
            assert!(dark.contains(&name), "{token} is missing from dark");
        }
    }

    /// **If this breaks:** two diagrams on one page collide, because SVG
    /// `<style>` is document-scoped rather than element-scoped and the second
    /// definition of `.box` silently wins for both.
    #[test]
    fn class_names_carry_the_diagram_prefix() {
        let s = style("loop");
        assert!(s.contains(".loop-box"), "{s}");
        assert!(!s.contains(" .box"), "an unprefixed class would leak: {s}");
    }
}
