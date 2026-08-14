//! Documents: turning a path into a URI, and turning "the symbol `Config` on
//! line 42" into the position LSP wants.
//!
//! Two conversions live here, and both are the kind that work perfectly on
//! every ASCII test and are wrong in production.
//!
//! **URIs.** LSP identifies files by `file://` URI, and Windows makes that
//! interesting: `C:\src\emma\src\x.rs` is `file:///e:/Projects/emma/src/x.rs`
//! — three slashes, forward slashes, and a drive letter that rust-analyzer
//! lowercases in everything it sends back. So the comparison in the other
//! direction cannot be a string comparison, and [`from_uri`] parses rather than
//! strips. A path that fails to round-trip here does not produce an error; it
//! produces a reference list quietly missing every hit in that file.
//!
//! **Positions are UTF-16 code units.** Not bytes, not characters. That is the
//! protocol default and Emma deliberately does not negotiate anything else — see
//! the capability block in `client` — so it is implemented once, here, with the
//! tests that matter. On a line containing `let café = Config::new();` a byte
//! offset and a UTF-16 offset differ by one, and the server answers about the
//! character next door: a real answer, about the wrong symbol, with nothing
//! anywhere saying so.
//!
//! **How a model names a position at all.** It does not, reliably — asking a
//! model for a column offset produces a plausible number. So the tools take a
//! file, a 1-based line, and the *symbol's name*, and [`locate`] finds the
//! column by searching that line. Ambiguity is refused rather than guessed, the
//! same discipline `Edit` applies to its anchor: `foo(foo)` is two occurrences,
//! and picking the first would be a coin flip dressed as an answer. The escape
//! hatch is `occurrence`.
//!
//! The shape is chosen so that `Grep` output feeds straight in — `path:line:`
//! plus the name the model was searching for is exactly what it already has.

use std::path::{Path, PathBuf};

use emma_tool_api::ToolError;

// region: URIs
// ---------------------------------------------------------------------------
// URIs
//
// Both directions, and a percent-encoder small enough to read. A dependency
// was considered; `url` is in the workspace already. It was not taken because
// the round trip that matters is Windows-drive-shaped and would need the same
// tests either way, and because `Url::to_file_path` is the exact function whose
// behaviour on verbatim `\\?\` paths this crate would then have to learn.
// ---------------------------------------------------------------------------

/// Characters that survive unescaped in a path segment.
///
/// `/` is added by the caller, not here: it is a separator, and encoding it
/// would produce a URI naming one file with slashes in its name.
fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/' | b':')
}

/// A path as a `file://` URI.
///
/// Windows verbatim prefixes are stripped first. `path::root` canonicalises,
/// and on Windows canonicalising produces `\\?\E:\...`; handing that to a
/// language server produces `file:///%5C%5C%3F%5CE:/...`, which matches nothing
/// the server ever says back — so every answer comes home about files Emma
/// cannot recognise, and the result is an empty list.
pub fn to_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    // A UNC path's host is the URI's *authority*, so `\\server\share\x` is
    // `file://server/share/x` — two slashes, not three. Getting this wrong
    // produces a URI with an empty authority and a path beginning `//`, which
    // no server resolves to the same file.
    let (text, unc) = if let Some(rest) = text.strip_prefix("//?/UNC/") {
        (rest.to_string(), true)
    } else if let Some(rest) = text.strip_prefix("//?/") {
        (rest.to_string(), false)
    } else if let Some(rest) = text.strip_prefix("//") {
        (rest.to_string(), true)
    } else {
        (text, false)
    };

    let mut out = String::from("file://");
    // An absolute unix path already starts with `/`; a Windows path starts with
    // a drive letter and needs the third slash. A UNC host needs neither.
    if !unc && !text.starts_with('/') {
        out.push('/');
    }
    for byte in text.bytes() {
        if is_unreserved(byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A `file://` URI back to a path, or `None` for anything else.
///
/// `None` is a real case rather than a defensive one: rust-analyzer answers with
/// locations inside dependencies, and in a `.rlib` those have no file URI at
/// all. The caller drops them, which is correct — Emma cannot show a line it
/// cannot read, and it must not pretend a path exists.
pub fn from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file://host/share/x` is a UNC path; `file:///x` has an empty authority.
    let rest = match rest.strip_prefix('/') {
        Some(after) => after.to_string(),
        None => format!("//{rest}"),
    };
    let decoded = percent_decode(&rest)?;

    // A Windows path is `e:/...`; a unix path lost its leading slash above.
    let looks_windows = decoded
        .as_bytes()
        .get(1)
        .is_some_and(|b| *b == b':' && decoded.as_bytes()[0].is_ascii_alphabetic());
    let text = if looks_windows || decoded.starts_with("//") {
        decoded
    } else {
        format!("/{decoded}")
    };
    Some(PathBuf::from(if cfg!(windows) {
        text.replace('/', "\\")
    } else {
        text
    }))
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = input.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

// endregion: URIs

// region: Positions
// ---------------------------------------------------------------------------
// Positions
//
// UTF-16 both ways, because that is what the protocol says and what the server
// will assume. Both directions exist because a request goes out as a position
// and an answer comes back as one, and getting only one of them right is worse
// than getting neither: the query lands on the right symbol and the result is
// reported at the wrong column.
// ---------------------------------------------------------------------------

/// Where a symbol is, in the protocol's units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// 0-based, as LSP counts.
    pub line: u32,
    /// 0-based, in UTF-16 code units.
    pub character: u32,
}

/// UTF-16 code units before `byte_offset` in `line`.
pub fn utf16_column(line: &str, byte_offset: usize) -> u32 {
    line[..byte_offset.min(line.len())]
        .chars()
        .map(|c| c.len_utf16() as u32)
        .sum()
}

/// The inverse: a byte offset from a UTF-16 column.
///
/// Clamps past the end of the line rather than failing. A server reporting a
/// column beyond the line means the file moved underneath it, and the useful
/// answer is "the end of this line" plus the readiness caveat — not an error
/// about an off-by-one in a line the model can see.
pub fn byte_offset(line: &str, utf16_col: u32) -> usize {
    let mut seen = 0u32;
    for (offset, c) in line.char_indices() {
        if seen >= utf16_col {
            return offset;
        }
        seen += c.len_utf16() as u32;
    }
    line.len()
}

/// Find `symbol` on 1-based `line_number` of `text`, and say where it is.
///
/// The refusals are the interesting part:
///
/// - A line number past the end of the file is `BadArguments`, naming how many
///   lines there are — a model that is one off fixes that immediately.
/// - The symbol not being on that line is `BadArguments` quoting the line, so
///   the model can see what is actually there. This is the common failure after
///   an `Edit` moved things, and showing the line turns a retry into a correction.
/// - More than one occurrence and no `occurrence` given is `BadArguments`
///   saying how many there are. Not a guess: in `foo(foo)` the two are a call
///   and an argument, and answering about the wrong one produces a confident,
///   wrong, unfalsifiable result.
pub fn locate(
    text: &str,
    line_number: u64,
    symbol: &str,
    occurrence: Option<u64>,
) -> Result<(Position, String), ToolError> {
    if line_number == 0 {
        return Err(ToolError::BadArguments(
            "line is 1-based; there is no line 0".into(),
        ));
    }
    let lines: Vec<&str> = text.lines().collect();
    let Some(line) = lines.get((line_number - 1) as usize) else {
        return Err(ToolError::BadArguments(format!(
            "line {line_number} is past the end of the file, which has {} line{}",
            lines.len(),
            if lines.len() == 1 { "" } else { "s" }
        )));
    };

    let hits: Vec<usize> = line.match_indices(symbol).map(|(i, _)| i).collect();
    if hits.is_empty() {
        return Err(ToolError::BadArguments(format!(
            "{symbol:?} does not appear on line {line_number}, which reads: {}",
            line.trim()
        )));
    }
    let index = match occurrence {
        None if hits.len() > 1 => {
            return Err(ToolError::BadArguments(format!(
                "{symbol:?} appears {} times on line {line_number}; pass occurrence (1-{}) to \
                 say which. The line reads: {}",
                hits.len(),
                hits.len(),
                line.trim()
            )))
        }
        None => 0,
        Some(0) => {
            return Err(ToolError::BadArguments(
                "occurrence is 1-based; there is no occurrence 0".into(),
            ))
        }
        Some(n) if (n as usize) > hits.len() => {
            return Err(ToolError::BadArguments(format!(
                "occurrence {n} was asked for, but {symbol:?} appears {} time{} on line \
                 {line_number}",
                hits.len(),
                if hits.len() == 1 { "" } else { "s" }
            )))
        }
        Some(n) => (n - 1) as usize,
    };

    Ok((
        Position {
            line: (line_number - 1) as u32,
            character: utf16_column(line, hits[index]),
        },
        (*line).to_string(),
    ))
}

// endregion: Positions

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Every one of these is a bug that produces a wrong *answer* rather than an
// error: a URI that matches nothing, a column that lands on the neighbouring
// character, a symbol chosen by coin flip. None of them would fail loudly in
// production.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_path_round_trips_through_a_uri() {
        let uri = to_uri(Path::new(r"C:\src\emma\src\main.rs"));
        assert_eq!(uri, "file:///C:/src/emma/src/main.rs");
        let back = from_uri(&uri).expect("round trip");
        let expected = if cfg!(windows) {
            PathBuf::from(r"C:\src\emma\src\main.rs")
        } else {
            PathBuf::from("C:/src/emma/src/main.rs")
        };
        assert_eq!(back, expected);
    }

    /// The one that produces an empty answer rather than an error.
    /// `path::root` canonicalises, and on Windows that yields `\\?\E:\...`.
    /// Encoded literally it becomes a URI naming a file that does not exist, the
    /// server indexes nothing under it, and every question returns `[]`.
    #[test]
    fn a_verbatim_windows_prefix_is_stripped_before_encoding() {
        assert_eq!(
            to_uri(Path::new(r"\\?\C:\src\emma")),
            "file:///C:/src/emma"
        );
        assert_eq!(
            to_uri(Path::new(r"\\?\UNC\server\share\x.rs")),
            "file://server/share/x.rs"
        );
        assert!(!to_uri(Path::new(r"\\?\E:\x")).contains("%3F"));
    }

    #[test]
    fn a_unix_path_round_trips_and_keeps_its_leading_slash() {
        let uri = to_uri(Path::new("/home/a/src/main.rs"));
        assert_eq!(uri, "file:///home/a/src/main.rs");
        // Asserted on both platforms rather than only off Windows. `from_uri`
        // rewrites `/` to `\` on Windows, but a `Path` comparison there is
        // component-wise and treats the two separators as one, so this is the
        // same claim on both — and the guard that used to be here meant the
        // decode half of the round trip was tested on exactly one platform.
        assert_eq!(
            from_uri(&uri).unwrap(),
            PathBuf::from("/home/a/src/main.rs")
        );
    }

    #[test]
    fn spaces_and_other_awkward_bytes_survive_both_ways() {
        let uri = to_uri(Path::new("/home/a/my project/ünïcode #1.rs"));
        assert!(uri.contains("%20"), "{uri}");
        assert!(uri.contains("%23"), "{uri}");
        assert!(!uri.contains(' '), "{uri}");
        // Both platforms, for the reason given above — and this is the one that
        // matters most, because percent-decoding is where a wrong answer is a
        // path that exists but is not the one asked about.
        assert_eq!(
            from_uri(&uri).unwrap(),
            PathBuf::from("/home/a/my project/ünïcode #1.rs")
        );
    }

    #[test]
    fn a_uri_that_is_not_a_file_is_none_rather_than_a_guess() {
        // rust-analyzer really does answer with these, for items inside
        // dependencies it only has metadata for.
        assert!(from_uri("jar:file:///x!/y.class").is_none());
        assert!(from_uri("untitled:Untitled-1").is_none());
        assert!(from_uri("rust-analyzer://synthetic").is_none());
    }

    /// The silent one. A byte offset and a UTF-16 offset are equal until the
    /// line has a non-ASCII character in it, and then the server answers about
    /// the character next door — a real answer, about the wrong symbol.
    #[test]
    fn columns_are_utf16_code_units_not_bytes_or_characters() {
        let line = "let café = Config::new();";
        let byte = line.find("Config").expect("present");
        assert_eq!(byte, 12, "é is two bytes, so the byte offset is 12");
        assert_eq!(utf16_column(line, byte), 11, "but eleven UTF-16 units");
        assert_eq!(byte_offset(line, 11), byte, "and the inverse agrees");

        // An astral character is two UTF-16 units and one char, which is where
        // "characters" and "code units" come apart in the other direction.
        let emoji = "let x = \"🦀\"; // Config";
        let byte = emoji.find("Config").expect("present");
        assert_eq!(byte, 19, "the crab is four bytes");
        assert_eq!(utf16_column(emoji, byte), 17, "but two UTF-16 units");
        assert_eq!(byte_offset(emoji, 17), byte);
    }

    #[test]
    fn a_column_past_the_end_of_a_line_clamps_rather_than_panicking() {
        assert_eq!(byte_offset("short", 500), 5);
        assert_eq!(utf16_column("short", 500), 5);
    }

    #[test]
    fn locate_finds_a_symbol_and_reports_the_line_it_found_it_on() {
        let text = "fn main() {\n    let c = Config::new();\n}\n";
        let (pos, line) = locate(text, 2, "Config", None).expect("found");
        assert_eq!(pos.line, 1, "LSP lines are 0-based");
        assert_eq!(pos.character, 12);
        assert_eq!(line, "    let c = Config::new();");
    }

    /// The refusal that keeps this from being a coin flip. Two occurrences and
    /// no way to tell them apart is not a case where guessing is 50% right — it
    /// is a case where a wrong answer is indistinguishable from a right one.
    #[test]
    fn an_ambiguous_symbol_is_refused_with_a_way_to_disambiguate() {
        let text = "    foo(foo);\n";
        let err = locate(text, 1, "foo", None).expect_err("ambiguous");
        assert_eq!(err.kind(), "bad_arguments");
        assert!(err.detail().contains("2 times"), "{err}");
        assert!(err.detail().contains("occurrence"), "{err}");

        assert_eq!(locate(text, 1, "foo", Some(1)).unwrap().0.character, 4);
        assert_eq!(locate(text, 1, "foo", Some(2)).unwrap().0.character, 8);
        assert!(locate(text, 1, "foo", Some(3)).is_err());
        assert!(locate(text, 1, "foo", Some(0)).is_err());
    }

    /// Both of these happen constantly after an `Edit` moved a line, so the
    /// message has to contain enough for the model to correct itself in one
    /// move rather than re-reading the file.
    #[test]
    fn the_misses_say_enough_to_correct_in_one_move() {
        let text = "one\ntwo\nthree\n";
        let err = locate(text, 9, "two", None).expect_err("past the end");
        assert!(err.detail().contains("3 lines"), "{err}");

        let err = locate(text, 1, "two", None).expect_err("not on that line");
        assert!(
            err.detail().contains("one"),
            "the line must be quoted: {err}"
        );

        assert!(locate(text, 0, "one", None).is_err(), "lines are 1-based");
    }
}

// endregion: Tests
