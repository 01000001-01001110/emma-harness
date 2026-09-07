//! Quoted source blocks for `docs/`, cut out of the file they cite.
//!
//! # What this replaces
//!
//! `DOCS.md` tells a page to quote the function it describes, exactly,
//! comments included, and to generate the quote rather than type it. Nothing
//! enforced either half. 93 pages carry 494 hand-written `<pre><code>` blocks,
//! and on 2026-09-07 one on `terminal-layout.html` was found quoting a
//! `regions()` with six identifiers that exist nowhere in the tree, attributed
//! to a file that does not contain it. The page had already caught the same
//! fabrication once, for a constant and a test, and missed the code block
//! beside it.
//!
//! A misquote inside a `<pre>` is the worst defect this site can carry,
//! because a block that looks copied gives the reader no reason to doubt it.
//!
//! # The arrangement
//!
//! The same one the diagrams use, and for the same reason. A page marks a
//! block with `<!-- quote:ID -->` … `<!-- /quote -->`; `all()` says which file
//! and which symbol that id names; `stale` regenerates every block and
//! `tests/current.rs` fails when a committed block differs from what the
//! source says now. `cargo run -p emma-docsgen` writes the update. Same shape
//! as `cargo fmt --check`, so a drifted quote is a red build rather than
//! something a reader has to notice.
//!
//! # What a block may be selected by
//!
//! A named item -- a `fn`, an inherent method, a `struct`, an `enum`, a
//! `const`, the module doc -- found by parsing the file with `syn`, never by
//! matching text. A line range would rot on the next insertion above it, and a
//! regex for `fn name` finds the call site as happily as the definition.
//!
//! `Sel::Region` narrows an item to a run of its lines between two anchors,
//! because half the useful quotes on this site are a branch inside a function
//! rather than a whole function. Both anchors must match exactly one line
//! inside the item, so an anchor that stops existing is an error rather than a
//! quietly shorter block.

use anyhow::{bail, Context, Result};
use std::path::Path;
use syn::spanned::Spanned;

/// What part of a file a block quotes.
#[derive(Debug, Clone)]
pub enum Sel {
    /// A free function, by name.
    Fn(&'static str),
    /// An inherent or trait method: the type the `impl` block is for, then the
    /// method name. The type is matched on the last path segment, so
    /// `impl Foo` and `impl fmt::Display for Foo` both answer to `Foo`.
    Method {
        ty: &'static str,
        name: &'static str,
    },
    /// A `struct`, with its fields and their doc comments.
    Struct(&'static str),
    /// An `enum`, with its variants.
    Enum(&'static str),
    /// A `const` or `static` item, with its doc comment.
    Const(&'static str),
    /// The file's leading `//!` block, from its first line to its last.
    ModuleDoc,
    /// Several selections from one file, joined by a blank line, in the order
    /// given. The blank line is the only thing the generator adds; each part
    /// is still verbatim.
    Group(Vec<Sel>),
    /// A run of lines inside another selection, from the line whose trimmed
    /// text starts with `first` to the line whose trimmed text starts with
    /// `last`. Each anchor must match exactly one line of the enclosing
    /// selection, and `last` must not come before `first`.
    Region {
        of: Box<Sel>,
        first: &'static str,
        last: &'static str,
    },
    /// One braced block inside another selection: the line whose trimmed text
    /// starts with `first`, through the line that closes it.
    ///
    /// `Region` cannot express this. The closing line of an `if` inside a
    /// function is `}`, and so are five other lines of that function, so the
    /// end anchor is ambiguous exactly where it is most wanted. The end is
    /// found by counting braces instead, with strings, char literals and
    /// comments skipped, so a `format!("{name}")` in the block does not move
    /// it.
    Block { of: Box<Sel>, first: &'static str },
}

/// One generated block: which page it lives on, the id in its marker, the
/// file it is cut from, and what part of that file.
#[derive(Debug, Clone)]
pub struct Quote {
    pub page: &'static str,
    pub id: &'static str,
    pub file: &'static str,
    pub sel: Sel,
}

/// Every quoted block this generator owns.
///
/// Adding one is two edits: put the marker pair on the page where the old
/// `<pre>` was, and add the row here. Nothing else has a list of these.
pub fn all() -> Vec<Quote> {
    let tools = "tools/lsp/src/tools.rs";
    let layout = "crates/emma/src/term/layout.rs";
    let theme = "crates/emma/src/term/theme.rs";
    vec![
        // --- tools-lsp.html ---------------------------------------------
        Quote {
            page: "tools-lsp",
            id: "cursor-schema",
            file: tools,
            sel: Sel::Group(vec![
                Sel::Const("FILE_PATH_DESCRIPTION"),
                Sel::Fn("cursor_schema"),
            ]),
        },
        Quote {
            page: "tools-lsp",
            id: "position-schema",
            file: tools,
            sel: Sel::Region {
                of: Box::new(Sel::Fn("position_schema")),
                first: "fn position_schema",
                last: "});",
            },
        },
        Quote {
            page: "tools-lsp",
            id: "rustup-proxy",
            file: "tools/lsp/src/server.rs",
            sel: Sel::Region {
                of: Box::new(Sel::ModuleDoc),
                first: "//! ```text",
                last: "//! reason a bundled",
            },
        },
        Quote {
            page: "tools-lsp",
            id: "init-options",
            file: "tools/lsp/src/lang.rs",
            sel: Sel::Const("RUST_INIT"),
        },
        // --- terminal-layout.html ---------------------------------------
        Quote {
            page: "terminal-layout",
            id: "regions",
            file: layout,
            sel: Sel::Fn("regions"),
        },
        Quote {
            page: "terminal-layout",
            id: "dock-height",
            file: layout,
            sel: Sel::Fn("dock_height"),
        },
        Quote {
            page: "terminal-layout",
            id: "latch",
            file: layout,
            sel: Sel::Group(vec![Sel::Struct("Latch"), Sel::Fn("hidden")]),
        },
        Quote {
            page: "terminal-layout",
            id: "new-session-hit",
            file: "crates/emma/src/term/app.rs",
            sel: Sel::Fn("new_session_hit"),
        },
        // --- terminal-themes.html ---------------------------------------
        Quote {
            page: "terminal-themes",
            id: "entry",
            file: theme,
            sel: Sel::Group(vec![
                Sel::Struct("Entry"),
                Sel::Struct("Theme"),
                Sel::Fn("slot"),
            ]),
        },
        Quote {
            page: "terminal-themes",
            id: "role-entry-tail",
            file: theme,
            sel: Sel::Region {
                of: Box::new(Sel::Fn("role_entry")),
                first: "Some(Entry {",
                last: "})",
            },
        },
        Quote {
            page: "terminal-themes",
            id: "text-is-not-settable",
            file: theme,
            sel: Sel::Block {
                of: Box::new(Sel::Fn("apply_roles")),
                first: "if name == \"text\"",
            },
        },
        Quote {
            page: "terminal-themes",
            id: "pair-collision",
            file: theme,
            sel: Sel::Block {
                of: Box::new(Sel::Fn("pair_halves")),
                first: "if halves[0].rgb == halves[1].rgb",
            },
        },
        // --- config-themes.html -----------------------------------------
        Quote {
            page: "config-themes",
            id: "theme-dirs",
            file: "crates/emma/src/session_command.rs",
            sel: Sel::Fn("theme_dirs"),
        },
        Quote {
            page: "config-themes",
            id: "load-signature",
            file: theme,
            sel: Sel::Region {
                of: Box::new(Sel::Fn("load")),
                first: "/// The theme in force",
                last: ") -> (Theme, Vec<String>) {",
            },
        },
        Quote {
            page: "config-themes",
            id: "reserved-name",
            file: theme,
            sel: Sel::Block {
                of: Box::new(Sel::Fn("load")),
                first: "if RESERVED.contains",
            },
        },
        Quote {
            page: "config-themes",
            id: "role-names",
            file: theme,
            sel: Sel::Group(vec![
                Sel::Const("ROLE_NAMES"),
                Sel::Const("SETTABLE"),
                Sel::Const("MAX_NOTICES"),
            ]),
        },
        Quote {
            page: "config-themes",
            id: "module-doc",
            file: theme,
            sel: Sel::Region {
                of: Box::new(Sel::ModuleDoc),
                first: "//! **Two things a theme may not do",
                last: "//! behind it",
            },
        },
        Quote {
            page: "config-themes",
            id: "apply-roles",
            file: theme,
            sel: Sel::Block {
                of: Box::new(Sel::Fn("apply_roles")),
                first: "for (name, spec) in roles",
            },
        },
        Quote {
            page: "config-themes",
            id: "role-entry-doc",
            file: theme,
            sel: Sel::Region {
                of: Box::new(Sel::Fn("role_entry")),
                first: "/// One role's three values",
                last: "/// thrown away over one character",
            },
        },
        Quote {
            page: "config-themes",
            id: "role-entry-hex",
            file: theme,
            sel: Sel::Region {
                of: Box::new(Sel::Fn("role_entry")),
                first: "let Some(rgb) = parse_hex",
                last: "})",
            },
        },
        Quote {
            page: "config-themes",
            id: "pair-halves-doc",
            file: theme,
            sel: Sel::Region {
                of: Box::new(Sel::Fn("pair_halves")),
                first: "/// Both halves of a pair",
                last: "/// not, and this repository",
            },
        },
        Quote {
            page: "config-themes",
            id: "pair-collision",
            file: theme,
            sel: Sel::Block {
                of: Box::new(Sel::Fn("pair_halves")),
                first: "if halves[0].rgb == halves[1].rgb",
            },
        },
    ]
}

/// The `<pre><code>` block for one quote, ready to sit between the markers.
pub fn render(root: &Path, q: &Quote) -> Result<String> {
    let path = root.join(q.file);
    let src = text(&path, &q.sel)
        .with_context(|| format!("{} quotes {} from {}", q.page, q.id, q.file))?;
    Ok(format!("<pre><code>{}</code></pre>", highlight(&src)))
}

/// The source a selection names, verbatim, with the block's common indent
/// removed.
///
/// Dedenting is uniform across the block rather than per line, so a method
/// quoted out of an `impl` reads at column zero while any relative indentation
/// inside it survives. Per-line stripping would hide drift, which is the
/// mistake `DOCS.md`'s check 2 was written after.
pub fn text(path: &Path, sel: &Sel) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("{} is quoted by a docs page", path.display()))?;
    let file =
        syn::parse_file(&raw).with_context(|| format!("{} did not parse", path.display()))?;
    let lines: Vec<&str> = raw.lines().collect();
    Ok(dedent(&select(&file, &lines, sel)?))
}

/// The lines a selection names, still carrying their file indentation.
fn select(file: &syn::File, lines: &[&str], sel: &Sel) -> Result<Vec<String>> {
    match sel {
        Sel::Group(parts) => {
            // **Where a gap is skipped, the gap is marked.** Two items joined
            // by a blank line read as adjacent, and the terminal chapter's
            // stricter pass in `DOCS.md` was written after two blocks were
            // found welding non-adjacent regions together with nothing to say
            // so. When the lines between two parts are not all blank, an
            // elision line goes in instead of the blank one.
            let mut out: Vec<String> = Vec::new();
            let mut previous_end: Option<usize> = None;
            for part in parts {
                let here = item_span(file, part).ok();
                if !out.is_empty() {
                    let contiguous = match (previous_end, here) {
                        (Some(end), Some((from, _))) => lines[end..from.saturating_sub(1)]
                            .iter()
                            .all(|l| l.trim().is_empty()),
                        // A part that is not one item cannot be placed, so the
                        // conservative answer is that something may be missing.
                        _ => false,
                    };
                    out.push(String::new());
                    if !contiguous {
                        out.push("// …".to_string());
                        out.push(String::new());
                    }
                }
                previous_end = here.map(|(_, to)| to);
                out.extend(select(file, lines, part)?);
            }
            Ok(out)
        }
        Sel::Region { of, first, last } => {
            let inner = select(file, lines, of)?;
            let at = |anchor: &str| -> Result<usize> {
                let hits: Vec<usize> = inner
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.trim_start().starts_with(anchor))
                    .map(|(i, _)| i)
                    .collect();
                match hits.len() {
                    1 => Ok(hits[0]),
                    0 => bail!("no line in the selection starts with `{anchor}`"),
                    n => bail!("`{anchor}` starts {n} lines of the selection; it must start one"),
                }
            };
            let (a, b) = (at(first)?, at(last)?);
            if b < a {
                bail!("`{last}` comes before `{first}` in the selection");
            }
            Ok(inner[a..=b].to_vec())
        }
        Sel::Block { of, first } => {
            let inner = select(file, lines, of)?;
            let hits: Vec<usize> = inner
                .iter()
                .enumerate()
                .filter(|(_, l)| l.trim_start().starts_with(first))
                .map(|(i, _)| i)
                .collect();
            let start = match hits.len() {
                1 => hits[0],
                0 => bail!("no line in the selection starts with `{first}`"),
                n => bail!("`{first}` starts {n} lines of the selection; it must start one"),
            };
            let depths = brace_depths(&inner);
            let base = if start == 0 { 0 } else { depths[start - 1] };
            // The brace may not be on the anchor line: rustfmt puts the `{` of
            // an `if` with a multi-line condition on a line of its own.
            let opened = (start..inner.len())
                .find(|i| depths[*i] > base)
                .with_context(|| format!("`{first}` opens no block"))?;
            let end = (opened + 1..inner.len())
                .find(|i| depths[*i] <= base)
                .with_context(|| format!("the block at `{first}` is never closed"))?;
            Ok(inner[start..=end].to_vec())
        }
        Sel::ModuleDoc => {
            let start = lines
                .iter()
                .position(|l| l.trim_start().starts_with("//!"))
                .context("the file has no `//!` module doc")?;
            let mut end = start;
            while end + 1 < lines.len() && lines[end + 1].trim_start().starts_with("//!") {
                end += 1;
            }
            Ok(lines[start..=end]
                .iter()
                .map(|l| (*l).to_string())
                .collect())
        }
        _ => {
            let (from, to) = item_span(file, sel)?;
            if to > lines.len() || from == 0 {
                bail!("the span for {sel:?} is outside the file");
            }
            Ok(lines[from - 1..to]
                .iter()
                .map(|l| (*l).to_string())
                .collect())
        }
    }
}

/// The 1-based, inclusive line range a named item occupies.
///
/// `syn`'s span for an item starts at its first token, and a `///` comment is
/// tokenised as a `#[doc]` attribute of the item, so the doc comment comes
/// with it. That is what a quote wants: this project puts the argument in the
/// comment and a quote without it is the code with the reasoning cut off.
fn item_span(file: &syn::File, sel: &Sel) -> Result<(usize, usize)> {
    let named = |name: &str, item: &syn::Item| -> bool {
        match (sel, item) {
            (Sel::Fn(_), syn::Item::Fn(f)) => f.sig.ident == name,
            (Sel::Struct(_), syn::Item::Struct(s)) => s.ident == name,
            (Sel::Enum(_), syn::Item::Enum(e)) => e.ident == name,
            (Sel::Const(_), syn::Item::Const(c)) => c.ident == name,
            (Sel::Const(_), syn::Item::Static(s)) => s.ident == name,
            _ => false,
        }
    };
    let span = match sel {
        Sel::Fn(name) | Sel::Struct(name) | Sel::Enum(name) | Sel::Const(name) => file
            .items
            .iter()
            .find(|i| named(name, i))
            .map(Spanned::span)
            .with_context(|| format!("{sel:?} names nothing in this file"))?,
        Sel::Method { ty, name } => file
            .items
            .iter()
            .filter_map(|i| match i {
                syn::Item::Impl(b) => Some(b),
                _ => None,
            })
            .filter(|b| self_ty_is(b, ty))
            .flat_map(|b| b.items.iter())
            .find_map(|i| match i {
                syn::ImplItem::Fn(f) if f.sig.ident == name => Some(f.span()),
                _ => None,
            })
            .with_context(|| format!("no `{ty}::{name}` in this file"))?,
        Sel::Group(_) | Sel::Region { .. } | Sel::Block { .. } | Sel::ModuleDoc => {
            bail!("{sel:?} is not a single item")
        }
    };
    Ok((span.start().line, span.end().line))
}

/// Whether an `impl` block is for the named type, by its last path segment.
fn self_ty_is(block: &syn::ItemImpl, ty: &str) -> bool {
    match &*block.self_ty {
        syn::Type::Path(p) => p.path.segments.last().is_some_and(|s| s.ident == ty),
        _ => false,
    }
}

/// The brace nesting depth after each line, with braces inside strings, char
/// literals and comments ignored.
///
/// The same shapes the highlighter knows about, for the same reason: a `{` in
/// a `format!` string or in a doc comment is not a block, and counting it puts
/// the end of a quoted branch several lines past where it is.
fn brace_depths(lines: &[String]) -> Vec<i32> {
    let text = lines.join(
        "
",
    );
    let b: Vec<char> = text.chars().collect();
    let code = code_mask(&b);
    let mut out = Vec::with_capacity(lines.len());
    let mut depth = 0i32;
    for (i, c) in b.iter().enumerate() {
        match c {
            '\n' => out.push(depth),
            '{' if code[i] => depth += 1,
            '}' if code[i] => depth -= 1,
            _ => {}
        }
    }
    out.push(depth);
    out.truncate(lines.len());
    while out.len() < lines.len() {
        out.push(depth);
    }
    out
}

/// Which characters are code rather than comment, string or char literal.
///
/// One pass over the whole selection rather than one per line, because a
/// string can span lines -- `theme.rs` has one held together with a trailing
/// backslash -- and a per-line scanner starts the next line believing it is in
/// code. The recognisers are the ones `highlight` uses.
fn code_mask(b: &[char]) -> Vec<bool> {
    let mut mask = vec![true; b.len()];
    let mut i = 0;
    while i < b.len() {
        let hide = |from: usize, to: usize, mask: &mut Vec<bool>| {
            for m in mask.iter_mut().take(to.min(b.len())).skip(from) {
                *m = false;
            }
        };
        if b[i] == '/' && b.get(i + 1) == Some(&'/') {
            let end = b[i..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(b.len(), |n| i + n);
            hide(i, end, &mut mask);
            i = end;
            continue;
        }
        if b[i] == '/' && b.get(i + 1) == Some(&'*') {
            let mut end = i + 2;
            let mut depth = 1usize;
            while end < b.len() && depth > 0 {
                if b[end] == '/' && b.get(end + 1) == Some(&'*') {
                    depth += 1;
                    end += 2;
                } else if b[end] == '*' && b.get(end + 1) == Some(&'/') {
                    depth -= 1;
                    end += 2;
                } else {
                    end += 1;
                }
            }
            hide(i, end, &mut mask);
            i = end;
            continue;
        }
        if let Some(end) = raw_string_at(b, i) {
            hide(i, end, &mut mask);
            i = end;
            continue;
        }
        if b[i] == '"' {
            let mut end = i + 1;
            while end < b.len() {
                match b[end] {
                    '\\' => end += 2,
                    '"' => {
                        end += 1;
                        break;
                    }
                    _ => end += 1,
                }
            }
            hide(i, end.min(b.len()), &mut mask);
            i = end.min(b.len());
            continue;
        }
        if b[i] == '\'' {
            if let Some(end) = char_literal_at(b, i) {
                hide(i, end, &mut mask);
                i = end;
                continue;
            }
        }
        i += 1;
    }
    mask
}

/// Remove the indentation every non-blank line shares.
fn dedent(lines: &[String]) -> String {
    let pad = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| if l.len() >= pad { &l[pad..] } else { l.trim() })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The strict keywords, the literals `true`/`false`, and the primitive type
/// names. The pages already colour `const`, `fn`, `str` and `false` this way;
/// this is that convention written down rather than a second one.
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64",
    "i128", "isize", "str", "u8", "u16", "u32", "u64", "u128", "usize",
];

/// Escape one character run for HTML. Only the three characters that can
/// change how a browser parses the block.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Wrap Rust source in the three span classes `docs.css` defines: `c` for a
/// comment, `s` for a string or char literal, `k` for a keyword.
///
/// A character scanner rather than `syn`: a highlighter has to see the
/// comments and the whitespace, which are exactly what a token stream throws
/// away. It is small because it only has to be right about the three things
/// the stylesheet colours, and it is total -- anything it does not recognise
/// comes out escaped and unwrapped.
pub fn highlight(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len() + src.len() / 4);
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // A line comment runs to the end of the line; `///` and `//!` are
        // comments to the reader whatever the tokeniser calls them.
        if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            let end = b[i..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(b.len(), |n| i + n);
            span(&mut out, "c", &b[i..end]);
            i = end;
            continue;
        }
        if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            let mut end = i + 2;
            let mut depth = 1usize;
            while end < b.len() && depth > 0 {
                if b[end] == '/' && end + 1 < b.len() && b[end + 1] == '*' {
                    depth += 1;
                    end += 2;
                } else if b[end] == '*' && end + 1 < b.len() && b[end + 1] == '/' {
                    depth -= 1;
                    end += 2;
                } else {
                    end += 1;
                }
            }
            span(&mut out, "c", &b[i..end]);
            i = end;
            continue;
        }
        // A raw string: `r`, some hashes, a quote, and the same hashes again.
        if (c == 'r' || c == 'b') && raw_string_at(&b, i).is_some() {
            let end = raw_string_at(&b, i).expect("just checked");
            span(&mut out, "s", &b[i..end]);
            i = end;
            continue;
        }
        if c == '"' {
            let mut end = i + 1;
            while end < b.len() {
                match b[end] {
                    '\\' => end += 2,
                    '"' => {
                        end += 1;
                        break;
                    }
                    _ => end += 1,
                }
            }
            span(&mut out, "s", &b[i..end.min(b.len())]);
            i = end.min(b.len());
            continue;
        }
        // A char literal, and not a lifetime: `'a'` and `'\n'` are literals,
        // `'a` in `&'a str` is not.
        if c == '\'' {
            if let Some(end) = char_literal_at(&b, i) {
                span(&mut out, "s", &b[i..end]);
                i = end;
                continue;
            }
        }
        if c.is_alphabetic() || c == '_' {
            let mut end = i;
            while end < b.len() && (b[end].is_alphanumeric() || b[end] == '_') {
                end += 1;
            }
            let word: String = b[i..end].iter().collect();
            if KEYWORDS.contains(&word.as_str()) {
                out.push_str(&format!("<span class=\"k\">{}</span>", escape(&word)));
            } else {
                out.push_str(&escape(&word));
            }
            i = end;
            continue;
        }
        out.push_str(&escape(&c.to_string()));
        i += 1;
    }
    out
}

/// Append `chars` wrapped in a span of `class`.
fn span(out: &mut String, class: &str, chars: &[char]) {
    let text: String = chars.iter().collect();
    out.push_str(&format!("<span class=\"{class}\">{}</span>", escape(&text)));
}

/// The index just past a raw string starting at `i`, or `None` if one does not
/// start there.
fn raw_string_at(b: &[char], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if b.get(i) == Some(&'b') {
        if b.get(j) != Some(&'r') {
            return None;
        }
        j += 1;
    }
    let hashes = {
        let start = j;
        while b.get(j) == Some(&'#') {
            j += 1;
        }
        j - start
    };
    if b.get(j) != Some(&'"') {
        return None;
    }
    j += 1;
    let close: String = std::iter::once('"')
        .chain(std::iter::repeat_n('#', hashes))
        .collect();
    let rest: String = b[j..].iter().collect();
    let at = rest.find(&close)?;
    Some(j + rest[..at].chars().count() + close.chars().count())
}

/// The index just past a char literal starting at `i`, or `None` when the
/// quote opens a lifetime instead.
fn char_literal_at(b: &[char], i: usize) -> Option<usize> {
    if b.get(i + 1) == Some(&'\\') {
        let mut j = i + 2;
        while j < b.len() && b[j] != '\'' {
            j += 1;
        }
        return (j < b.len()).then_some(j + 1);
    }
    (b.get(i + 2) == Some(&'\'')).then_some(i + 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"//! A module.
//! Second line.

use std::fmt;

/// What it does.
const N: usize = 3;

/// A function.
fn f(x: &str) -> bool {
    // a note
    x == "hi"
}

struct S {
    a: u8,
}

impl S {
    /// A method.
    fn m(&self) -> u8 {
        self.a
    }
}
"#;

    fn pick(sel: Sel) -> String {
        let file = syn::parse_file(SRC).expect("fixture parses");
        let lines: Vec<&str> = SRC.lines().collect();
        dedent(&select(&file, &lines, &sel).expect("the selection resolves"))
    }

    /// **If this breaks:** a quoted function arrives without the doc comment
    /// above it, which is where this project puts the argument the code cannot
    /// carry. The quote would then be the code with its reasoning cut off.
    #[test]
    fn a_function_is_quoted_with_its_doc_comment_and_body() {
        let out = pick(Sel::Fn("f"));
        assert!(out.starts_with("/// A function."), "{out}");
        assert!(out.contains("// a note"), "{out}");
        assert!(out.ends_with('}'), "{out}");
        assert!(!out.contains("struct S"), "{out}");
    }

    /// **If this breaks:** a method quoted out of an `impl` keeps the block's
    /// four spaces, so every line of the page's block is indented and any
    /// relative indent inside it is measured from the wrong column.
    #[test]
    fn a_method_is_dedented_as_a_block_and_not_line_by_line() {
        let out = pick(Sel::Method { ty: "S", name: "m" });
        assert_eq!(
            out, "/// A method.\nfn m(&self) -> u8 {\n    self.a\n}",
            "{out}"
        );
    }

    /// **If this breaks:** a page quoting the module doc gets the `use` lines
    /// under it too, or stops at the first line.
    #[test]
    fn the_module_doc_is_its_contiguous_run_of_bang_comments() {
        assert_eq!(pick(Sel::ModuleDoc), "//! A module.\n//! Second line.");
    }

    /// **If this breaks:** a region silently returns a shorter block when its
    /// anchor stops existing, which is the failure the whole mechanism is for:
    /// a page quietly quoting something other than what it says it quotes.
    #[test]
    fn a_region_anchor_that_matches_nothing_is_an_error() {
        let file = syn::parse_file(SRC).expect("fixture parses");
        let lines: Vec<&str> = SRC.lines().collect();
        let e = select(
            &file,
            &lines,
            &Sel::Region {
                of: Box::new(Sel::Fn("f")),
                first: "// gone",
                last: "}",
            },
        )
        .expect_err("the anchor is not there");
        assert!(e.to_string().contains("no line"), "{e}");
    }

    /// **If this breaks:** an anchor that matches two lines picks one of them
    /// by position, so an edit elsewhere in the function moves the block.
    #[test]
    fn an_ambiguous_region_anchor_is_an_error() {
        let file = syn::parse_file(SRC).expect("fixture parses");
        let lines: Vec<&str> = SRC.lines().collect();
        let e = select(
            &file,
            &lines,
            &Sel::Region {
                of: Box::new(Sel::Struct("S")),
                first: "",
                last: "}",
            },
        )
        .expect_err("the empty anchor matches every line");
        assert!(e.to_string().contains("must start one"), "{e}");
    }

    /// **If this breaks:** a name that no longer exists produces an empty
    /// block rather than a failure, and the page keeps whatever it last said.
    #[test]
    fn a_missing_symbol_is_an_error_rather_than_an_empty_block() {
        let file = syn::parse_file(SRC).expect("fixture parses");
        let lines: Vec<&str> = SRC.lines().collect();
        let e = select(&file, &lines, &Sel::Fn("gone")).expect_err("no such fn");
        assert!(e.to_string().contains("names nothing"), "{e}");
    }

    /// **If this breaks:** a `<` in a generic parameter or an `&` in a
    /// reference reaches the page raw and the browser eats the rest of the
    /// block.
    #[test]
    fn the_three_html_characters_are_escaped() {
        let out = highlight("fn f(x: &Vec<u8>) {}");
        assert!(out.contains("&amp;Vec&lt;"), "{out}");
        assert!(out.contains("&gt;) {}"), "{out}");
        assert!(!out.contains("<u8>"), "{out}");
    }

    /// **If this breaks:** the generated blocks stop matching the colouring
    /// the hand-written ones have, so a converted page looks different from
    /// the page beside it.
    #[test]
    fn comments_strings_and_keywords_get_the_classes_the_stylesheet_defines() {
        let out = highlight("const A: &str = \"hi\"; // why\n");
        assert!(out.contains("<span class=\"k\">const</span>"), "{out}");
        assert!(out.contains("<span class=\"s\">\"hi\"</span>"), "{out}");
        assert!(out.contains("<span class=\"c\">// why</span>"), "{out}");
    }

    /// **If this breaks:** a `//` inside a string literal starts a comment
    /// that runs to the end of the line, and a `"` inside a comment starts a
    /// string that runs to the next quote several lines down. Either one
    /// mangles the rest of the block.
    #[test]
    fn a_comment_marker_in_a_string_and_a_quote_in_a_comment_stay_put() {
        let out = highlight("let u = \"http://x\"; // a \"quoted\" word\nlet n = 1;");
        assert!(
            out.contains("<span class=\"s\">\"http://x\"</span>"),
            "{out}"
        );
        assert!(out.contains("<span class=\"k\">let</span> n"), "{out}");
    }

    /// **If this breaks:** the `'a` in `&'a str` is read as an unterminated
    /// char literal and everything after it on the line turns into a string.
    #[test]
    fn a_lifetime_is_not_a_char_literal() {
        let out = highlight("fn f<'a>(s: &'a str) -> char { 'x' }");
        assert!(out.contains("<span class=\"k\">str</span>"), "{out}");
        assert!(out.contains("<span class=\"s\">'x'</span>"), "{out}");
    }

    /// **If this breaks:** every id in `all()` is not unique per page, so two
    /// blocks share a marker and the second overwrites the first.
    #[test]
    fn every_quote_has_its_own_marker_on_its_page() {
        let mut seen: Vec<(&str, &str)> = all().iter().map(|q| (q.page, q.id)).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(before, seen.len(), "two quotes share a marker: {seen:?}");
    }
}
