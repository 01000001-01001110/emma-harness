//! What the model is shown, and the two rules that shape all of it.
//!
//! **A location is useless without its line.** "src/config.rs:412:9" makes the
//! model open the file. "src/config.rs:412  `let c = Config::new();`" is
//! usually the whole answer, and where it is not, it is enough to choose which
//! three of forty hits are worth reading. So every location carries its source
//! line, and the cost of that — reading each hit file once — is why the cap
//! exists.
//!
//! **An empty result is only "none" when the server was ready.** This is the
//! rule the whole crate is arranged around, and it is enforced here, at the one
//! place a result becomes prose. `Ok` with an empty list is the correct contract
//! answer — emptiness is a fact about the world — but *only if the world was
//! actually consulted*. When readiness is anything but `Ready`, the sentence
//! says the question could not be answered rather than that the answer is none,
//! and `no_results_line` is where those two sentences are chosen between.
//!
//! Locations outside the working directory are counted and named, never shown
//! and never silently dropped. rust-analyzer answers with hits in `~/.cargo/
//! registry` and in the standard library as a matter of course; Emma's
//! containment says it does not read those, so the honest rendering is "and 6
//! more outside the working directory" rather than a shorter list that looks
//! complete.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use emma_tool_api::ToolOutcome;
use emma_tools_fs::path;
use serde_json::Value;

use crate::client::Readiness;
use crate::doc;
use crate::doc::Position;
use crate::server::Server;

// region: Caps
// ---------------------------------------------------------------------------
// Caps
//
// The same shape `tools/fs` uses: a ceiling, and a line in the output saying
// the ceiling was reached. Silent truncation is worse than a short answer,
// because the model reasons confidently about the part it never saw.
// ---------------------------------------------------------------------------

/// Locations shown before the list is cut.
///
/// A hundred, chosen against what the output is for: twenty references with
/// their source lines is a thing a model reads and acts on; two hundred is a
/// thing it summarises badly. Past this the useful move is a narrower question,
/// and the truncation note says the count so it can tell.
pub const MAX_LOCATIONS: usize = 100;

/// Symbols shown by `DocumentSymbols` before the list is cut.
pub const MAX_SYMBOLS: usize = 300;

/// Longest source line echoed. Generated code has lines in the tens of
/// thousands of characters and one of them would fill the whole result.
const MAX_LINE_CHARS: usize = 300;

// endregion: Caps

// region: One location
// ---------------------------------------------------------------------------
// One location
//
// Parsing the three shapes LSP answers positions in, and holding onto only what
// the rendering needs.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    /// 0-based, as LSP counts. Rendered 1-based.
    pub line: u32,
    pub character: u32,
}

/// Pull locations out of whatever the server answered.
///
/// Three shapes, because `textDocument/definition` may reply with any of them
/// and the choice is the server's: a single `Location`, an array of them, or an
/// array of `LocationLink`. Handling one and returning nothing for the others
/// would look exactly like "there is no definition".
pub fn parse_locations(value: &Value) -> Vec<Location> {
    fn one(v: &Value, out: &mut Vec<Location>) {
        // `LocationLink` first: it has `targetUri`, and its
        // `targetSelectionRange` is the identifier rather than the whole item,
        // which is the more useful of the two ranges it carries.
        let (uri, range) = if let Some(uri) = v.get("targetUri").and_then(Value::as_str) {
            (
                uri,
                v.get("targetSelectionRange")
                    .or_else(|| v.get("targetRange")),
            )
        } else if let Some(uri) = v.get("uri").and_then(Value::as_str) {
            (uri, v.get("range"))
        } else {
            return;
        };
        let Some(path) = doc::from_uri(uri) else {
            // A non-file URI — an item inside a dependency Emma has only
            // metadata for. Dropped rather than rendered as a path that is not
            // one; the count of what was dropped is not tracked because these
            // are indistinguishable from the out-of-root hits already counted.
            return;
        };
        let start = range.and_then(|r| r.get("start"));
        out.push(Location {
            path,
            line: start
                .and_then(|s| s.get("line"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            character: start
                .and_then(|s| s.get("character"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
        });
    }

    let mut out = Vec::new();
    match value {
        Value::Array(items) => {
            for item in items {
                one(item, &mut out);
            }
        }
        Value::Object(_) => one(value, &mut out),
        _ => {}
    }
    out
}

// endregion: One location

// region: Rendering
// ---------------------------------------------------------------------------
// Rendering
//
// The banner, the caveat, the containment filter, the grouping, the cap. In
// that order, because that is the order a model reads them in and the first two
// change how the rest should be read.
// ---------------------------------------------------------------------------

/// The first lines of every result: which server answered, and whether it was
/// in a state where its answer means anything.
///
/// The banner is here rather than in the description for the reason
/// `Bash::banner` is: the description is hashed into `tool_schema_hash`, and a
/// description naming the local binary would make that hash a property of the
/// machine.
pub fn header(server: &Server, readiness: Readiness, health: Option<&str>) -> String {
    let mut out = server.banner();
    // Health before readiness, because it is the worse news: a server that has
    // finished and cannot load the project is `Ready` and answers nothing, which
    // is the one case readiness alone gets exactly backwards.
    if let Some(health) = health {
        out.push('\n');
        out.push_str(health);
    }
    if let Some(caveat) = readiness.caveat() {
        out.push('\n');
        out.push_str(caveat);
    }
    // The language's own known hole, from the table. Rust's is proc macros;
    // terraform's is an uninitialised directory; ansible's is a missing
    // `ansible-lint`. Each of them is a way for a short answer to be wrong in
    // the confident direction, which is the only direction this crate is
    // dangerous in.
    if !server.language.caveat.is_empty() {
        out.push('\n');
        out.push_str(server.language.caveat);
    }
    out
}

/// The sentence for a result with nothing in it.
///
/// The whole crate turns on this function. "No references found" is a claim
/// about the codebase; it may only be made when the index that was consulted was
/// finished. Otherwise the honest sentence says the question was not answered —
/// and says what to do about it, because a model that is told only "unknown"
/// will move on rather than wait.
pub fn no_results_line(what: &str, readiness: Readiness) -> String {
    if readiness.is_ready() {
        format!(
            "No {what} found. The index was complete when this was asked, so this is an \
             answer: there are none."
        )
    } else {
        format!(
            "No {what} came back, and the index was not complete — so this is not an answer, \
             it is a question that could not be answered yet. Do not conclude that none \
             exist; ask again once indexing has finished."
        )
    }
}

/// Locations, grouped by file, each with its source line.
pub fn locations(
    root: &Path,
    server: &Server,
    readiness: Readiness,
    health: Option<&str>,
    what: &str,
    found: Vec<Location>,
) -> ToolOutcome {
    // Containment, before anything is read or counted as a result. A language
    // server answers about the standard library and about every dependency's
    // source; Emma has decided it does not read outside the root, and that
    // decision has to hold for a path that arrived over a pipe exactly as it
    // does for one the model typed.
    let total = found.len();
    let inside: Vec<Location> = found
        .into_iter()
        .filter_map(|loc| {
            // Canonicalised before the comparison, and this is not belt and
            // braces — it is the check working at all. `root` comes from
            // `path::root`, which canonicalises, and on Windows that is the
            // `\\?\E:\…` verbatim form; a URI round trip produces the ordinary
            // `E:\…` spelling. Comparing the two silently never matches, so
            // every hit in the project reads as a hit outside it and the tool
            // reports "no references, and 14 in dependencies". Caught by
            // `tests/tools.rs`, which is the only reason it is not still here.
            //
            // Canonicalising also makes containment mean here exactly what it
            // means in `path::resolve`: a symlink the server answered through
            // is resolved before it is judged, rather than after.
            let resolved = loc.path.canonicalize().unwrap_or_else(|_| loc.path.clone());
            resolved.starts_with(root).then_some(Location {
                path: resolved,
                ..loc
            })
        })
        .collect();
    let outside = total - inside.len();

    let mut body = String::new();
    if inside.is_empty() {
        body.push_str(&no_results_line(what, readiness));
    } else {
        // Sorted and grouped so the same question gives the same answer twice —
        // the server's order is its own and is not stable across runs.
        let mut by_file: BTreeMap<PathBuf, Vec<Location>> = BTreeMap::new();
        for loc in inside.iter().take(MAX_LOCATIONS) {
            by_file
                .entry(loc.path.clone())
                .or_default()
                .push(loc.clone());
        }
        let shown: usize = by_file.values().map(Vec::len).sum();
        body.push_str(&format!(
            "{shown} {what} in {} file{}:\n",
            by_file.len(),
            if by_file.len() == 1 { "" } else { "s" }
        ));
        for (file, mut locs) in by_file {
            locs.sort_by_key(|l| (l.line, l.character));
            body.push_str(&format!("\n{}\n", path::display(root, &file)));
            // One read per file rather than per hit. A file with forty
            // references in it is the normal case for the tool that matters
            // most, and reading it forty times is the obvious way to make this
            // slow enough to notice.
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            for loc in locs {
                let text = lines
                    .get(loc.line as usize)
                    .map(|l| clip(l.trim_end()))
                    // The file moved between the server indexing it and Emma
                    // reading it. Said rather than shown as a blank line,
                    // because a blank line reads as an empty source line.
                    .unwrap_or_else(|| "(this line is no longer in the file)".to_string());
                body.push_str(&format!("  {}: {text}\n", loc.line + 1));
            }
        }
    }

    let capped = inside.len() > MAX_LOCATIONS;
    if capped {
        body.push_str(&format!(
            "\n[truncated: {MAX_LOCATIONS} of {} shown. Ask a narrower question rather than \
             assuming the rest are like these.]\n",
            inside.len()
        ));
    }
    if outside > 0 {
        // Named, not dropped. A list that silently omits six hits in the
        // standard library reads as a complete list with six fewer entries.
        body.push_str(&format!(
            "\n{outside} further {what} are outside the working directory — in dependencies or \
             the standard library — and are not shown.\n"
        ));
    }

    let outcome = ToolOutcome::new(format!("{}\n{body}", header(server, readiness, health)))
        .with_display(format!("{} {what}", inside.len()));
    if capped {
        // `truncated_because`, not the bare `truncated()`. `tool-api` calls the
        // bare form "the weaker form … never the preferred one for a new tool",
        // and it was in use here: the flag reached the model with no reason
        // attached, so `agent.rs` appended its generic "this tool did not say
        // which limit cut it" fallback. The tool knew the cap and the loss all
        // along and was not passing them on.
        outcome.truncated_because(format!(
            "{MAX_LOCATIONS} of {} {what} shown; the rest are not here. No argument raises that \
             — ask a narrower question, or Grep for the symbol to see every hit",
            inside.len()
        ))
    } else {
        outcome
    }
}

/// A source line, clipped so one line of generated code cannot fill a result.
fn clip(line: &str) -> String {
    let trimmed = line.trim_start();
    if trimmed.chars().count() <= MAX_LINE_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(MAX_LINE_CHARS).collect();
    format!("{head}… [line clipped]")
}

/// Markdown or plain text out of a `Hover` reply, which has three legal shapes
/// and a deprecated fourth.
pub fn hover_text(value: &Value) -> Option<String> {
    let contents = value.get("contents")?;
    let text = match contents {
        Value::String(s) => s.clone(),
        Value::Object(o) => o.get("value").and_then(Value::as_str)?.to_string(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("value").and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return None,
    };
    Some(text).filter(|t| !t.trim().is_empty())
}

/// A document symbol tree, flattened with indentation.
///
/// Two shapes again: `DocumentSymbol` is a tree with `children`, and
/// `SymbolInformation` is a flat list with a `location`. rust-analyzer sends the
/// first because Emma declares `hierarchicalDocumentSymbolSupport`; the second
/// is handled because a server that ignores that declaration would otherwise
/// render as an empty file.
pub fn symbols(
    server: &Server,
    readiness: Readiness,
    health: Option<&str>,
    value: &Value,
) -> ToolOutcome {
    let mut lines: Vec<String> = Vec::new();
    fn walk(items: &Value, depth: usize, out: &mut Vec<String>) {
        let Some(items) = items.as_array() else {
            return;
        };
        for item in items {
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            let kind = symbol_kind(item.get("kind").and_then(Value::as_u64).unwrap_or(0));
            let line = item
                .get("range")
                .or_else(|| item.get("location").and_then(|l| l.get("range")))
                .and_then(|r| r.get("start"))
                .and_then(|s| s.get("line"))
                .and_then(Value::as_u64)
                .map(|l| l + 1);
            let detail = item
                .get("detail")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
                .map(|d| format!("  {d}"))
                .unwrap_or_default();
            out.push(format!(
                "{}{}: {name}{detail}{}",
                "  ".repeat(depth),
                kind,
                line.map(|l| format!("  (line {l})")).unwrap_or_default()
            ));
            if let Some(children) = item.get("children") {
                walk(children, depth + 1, out);
            }
        }
    }
    walk(value, 0, &mut lines);

    let capped = lines.len() > MAX_SYMBOLS;
    let body = if lines.is_empty() {
        no_results_line("symbols", readiness)
    } else {
        let mut body = lines
            .iter()
            .take(MAX_SYMBOLS)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        if capped {
            body.push_str(&format!(
                "\n[truncated: {MAX_SYMBOLS} of {} symbols shown]",
                lines.len()
            ));
        }
        body
    };

    let outcome = ToolOutcome::new(format!("{}\n{body}", header(server, readiness, health)))
        .with_display(format!("{} symbols", lines.len()));
    if capped {
        outcome.truncated_because(format!(
            "{MAX_SYMBOLS} of {} symbols shown; the rest are not here. No argument raises \
             that — narrow the query, or Grep the file to see every symbol in it",
            lines.len()
        ))
    } else {
        outcome
    }
}

/// LSP `SymbolKind` is an integer. Named rather than printed, because "6" tells
/// the model nothing and "method" tells it what it is looking at.
fn symbol_kind(kind: u64) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "struct",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "trait",
        12 => "fn",
        13 => "variable",
        14 => "const",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        22 => "enum member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type parameter",
        _ => "symbol",
    }
}

// region: Diagnostics
// ---------------------------------------------------------------------------
// Diagnostics
//
// Three outcomes and three sentences. The third one is why the tool exists in a
// crate that refused to ship it before.
// ---------------------------------------------------------------------------

/// How many diagnostics to print before saying how many were left out.
///
/// A generated file or a broken `terraform init` produces hundreds, and a tool
/// result that is four thousand lines of the same warning has spent the
/// context window to say one thing.
pub const MAX_DIAGNOSTICS: usize = 100;

fn severity(value: &Value) -> &'static str {
    match value.as_u64() {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "info",
        Some(4) => "hint",
        // Absent is legal and means the server did not say. Reported as such
        // rather than defaulted to "error", which would invent a severity.
        _ => "unspecified",
    }
}

/// One publication, rendered.
///
/// `items` is `None` when the server published nothing inside the wait, and
/// that is the case this whole function is arranged around: it gets a sentence
/// that cannot be read as "the file is clean", and it says how long the wait
/// was and which variable changes it.
pub fn diagnostics(
    root: &Path,
    file: &Path,
    server: &Server,
    readiness: Readiness,
    health: Option<&str>,
    waited: std::time::Duration,
    items: Option<Vec<Value>>,
) -> ToolOutcome {
    let head = header(server, readiness, health);
    let shown = path::display(root, file);

    let Some(items) = items else {
        return ToolOutcome::new(format!(
            "{head}\nThe language server published no diagnostics for {shown} within \
             {:.1}s. That is not a clean result: it means the server did not answer, not \
             that the file has no problems. Wait and ask again, or raise the wait with \
             {}.",
            waited.as_secs_f64(),
            crate::client::DIAGNOSTICS_WAIT_ENV
        ));
    };

    if items.is_empty() {
        return ToolOutcome::new(format!(
            "{head}\nThe language server reported no problems in {shown}. This one is an \
             answer: it published an empty diagnostic set for this file."
        ));
    }

    let total = items.len();
    let mut out = format!("{head}\n{total} diagnostic(s) in {shown}:");
    for item in items.iter().take(MAX_DIAGNOSTICS) {
        // 0-based on the wire, 1-based everywhere a human or a model reads it,
        // which is the same conversion `doc` does in the other direction.
        let line = item["range"]["start"]["line"].as_u64().unwrap_or(0) + 1;
        let column = item["range"]["start"]["character"].as_u64().unwrap_or(0) + 1;
        let source = item["source"].as_str().unwrap_or("");
        let code = match &item["code"] {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        };
        let tag = match (source.is_empty(), code.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!(" [{source}]"),
            (true, false) => format!(" [{code}]"),
            (false, false) => format!(" [{source}:{code}]"),
        };
        let message = item["message"].as_str().unwrap_or("").trim();
        out.push_str(&format!(
            "\n  {line}:{column} {}{tag}: {}",
            severity(&item["severity"]),
            clip(message)
        ));
    }
    // A cut that names the cap, the loss and the remedy, in the same shape the
    // two renderers above use. `truncated_because` and not the bare
    // `truncated()`: `tool-api` calls that the weaker form, and a flag with no
    // reason makes `agent.rs` apologise on the tool's behalf for a limit the
    // tool knew all along.
    if total > MAX_DIAGNOSTICS {
        return ToolOutcome::new(out).truncated_because(format!(
            "{MAX_DIAGNOSTICS} of {total} diagnostics shown; the rest are not here. No \
             argument raises that — fix these and ask again, since a server usually \
             reports far fewer once the first errors are gone"
        ));
    }
    ToolOutcome::new(out)
}

// endregion: Diagnostics

// endregion: Rendering

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The empty-result rule is the one that must never regress, so it is tested
// from both sides. The rest guard the two silent-wrong-answer failures:
// dropping a location shape, and letting an out-of-root hit through.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn server() -> Server {
        server_for("rust")
    }

    /// The fixture wearing one language's identity, because the table now
    /// decides the banner's label and the caveat every result carries.
    fn server_for(key: &str) -> Server {
        Server {
            program: PathBuf::from("/usr/bin/a-language-server"),
            args: Vec::new(),
            entry: PathBuf::from("/usr/bin/a-language-server"),
            version: "0.3.0".into(),
            source: crate::server::Source::Path,
            language: crate::lang::by_key(key).expect("a language in the table"),
        }
    }

    /// The rule the crate exists for, stated as a test in both directions. A
    /// ready empty result is an answer; an unready one is not, and must not read
    /// like one.
    #[test]
    fn an_empty_result_only_says_none_when_the_index_was_complete() {
        let ready = no_results_line("references", Readiness::Ready);
        assert!(ready.contains("there are none"), "{ready}");

        for state in [
            Readiness::Indexing,
            Readiness::Unknown,
            Readiness::Handshaking,
        ] {
            let line = no_results_line("references", state);
            assert!(
                !line.contains("there are none"),
                "{state:?} must not claim there are none: {line}"
            );
            assert!(line.contains("not an answer"), "{state:?}: {line}");
            assert!(line.contains("Do not conclude"), "{state:?}: {line}");
        }
    }

    /// And the same rule survives the trip through the real rendering path,
    /// which is where a future edit would most plausibly bypass it.
    #[test]
    fn the_rendered_empty_result_carries_the_caveat_twice_over() {
        let root = PathBuf::from("/work");
        let out = locations(
            &root,
            &server(),
            Readiness::Indexing,
            None,
            "references",
            vec![],
        );
        assert!(out.content.contains("still indexing"), "{}", out.content);
        assert!(out.content.contains("not an answer"), "{}", out.content);
        assert!(!out.content.contains("there are none"), "{}", out.content);

        let out = locations(
            &root,
            &server(),
            Readiness::Ready,
            None,
            "references",
            vec![],
        );
        assert!(out.content.contains("there are none"), "{}", out.content);
        // A caveat on a correct answer is noise, and noise teaches the model to
        // skip caveats — including the one that matters.
        assert!(!out.content.contains("not evidence"), "{}", out.content);
    }

    /// Containment holds for paths that arrive over a pipe. rust-analyzer
    /// answers with hits in `~/.cargo/registry` constantly, and a filter that
    /// dropped them silently would produce a list that reads as complete.
    #[test]
    fn hits_outside_the_root_are_counted_and_named_not_silently_dropped() {
        let root = PathBuf::from("/work");
        let found = vec![
            Location {
                path: PathBuf::from("/work/src/a.rs"),
                line: 0,
                character: 0,
            },
            Location {
                path: PathBuf::from("/home/a/.cargo/registry/src/serde/lib.rs"),
                line: 9,
                character: 0,
            },
            Location {
                path: PathBuf::from("/rustlib/src/core/option.rs"),
                line: 9,
                character: 0,
            },
        ];
        let out = locations(
            &root,
            &server(),
            Readiness::Ready,
            None,
            "references",
            found,
        );
        assert!(out.content.contains("2 further"), "{}", out.content);
        assert!(
            out.content.contains("outside the working directory"),
            "{}",
            out.content
        );
        assert!(
            !out.content.contains(".cargo"),
            "an outside path leaked: {}",
            out.content
        );
        // The sibling-prefix trap `path::resolve` documents: `/workspace` is not
        // inside `/work`, and a string prefix test would say it was.
        let out = locations(
            &root,
            &server(),
            Readiness::Ready,
            None,
            "references",
            vec![Location {
                path: PathBuf::from("/workspace/src/a.rs"),
                line: 0,
                character: 0,
            }],
        );
        assert!(out.content.contains("1 further"), "{}", out.content);
    }

    /// Three legal reply shapes for one request. Handling one and returning
    /// nothing for the others is indistinguishable from "there is no
    /// definition", which is the failure mode this crate keeps having to guard.
    #[test]
    fn all_three_location_shapes_parse() {
        let single = json!({ "uri": "file:///work/a.rs", "range": { "start": { "line": 4, "character": 8 } } });
        assert_eq!(parse_locations(&single).len(), 1);
        assert_eq!(
            parse_locations(&json!([single.clone(), single.clone()])).len(),
            2
        );

        let link = json!([{
            "targetUri": "file:///work/b.rs",
            "targetRange": { "start": { "line": 0, "character": 0 } },
            "targetSelectionRange": { "start": { "line": 3, "character": 11 } },
        }]);
        let parsed = parse_locations(&link);
        assert_eq!(parsed.len(), 1);
        // The selection range, not the whole item: it points at the identifier.
        assert_eq!(parsed[0].line, 3);
        assert_eq!(parsed[0].character, 11);

        // And the shapes that carry nothing usable produce nothing rather than
        // a location at 0:0 in a path that does not exist.
        assert!(parse_locations(&Value::Null).is_empty());
        assert!(parse_locations(&json!([{ "uri": "rust-analyzer://synthetic" }])).is_empty());
    }

    #[test]
    fn hover_handles_every_shape_the_protocol_allows() {
        assert_eq!(
            hover_text(&json!({ "contents": "plain" })).as_deref(),
            Some("plain")
        );
        assert_eq!(
            hover_text(
                &json!({ "contents": { "kind": "markdown", "value": "```rust\nfn x()\n```" } })
            )
            .as_deref(),
            Some("```rust\nfn x()\n```")
        );
        assert_eq!(
            hover_text(&json!({ "contents": ["a", { "value": "b" }] })).as_deref(),
            Some("a\n\nb")
        );
        // Empty is `None` so the caller can say "no type information here"
        // rather than printing a blank result that reads as a broken tool.
        assert!(hover_text(&json!({ "contents": "  " })).is_none());
        assert!(hover_text(&Value::Null).is_none());
    }

    #[test]
    fn symbols_render_as_a_tree_with_named_kinds() {
        let value = json!([{
            "name": "Config", "kind": 23,
            "range": { "start": { "line": 4, "character": 0 } },
            "children": [{
                "name": "new", "kind": 6, "detail": "fn() -> Config",
                "range": { "start": { "line": 9, "character": 4 } },
            }],
        }]);
        let out = symbols(&server(), Readiness::Ready, None, &value);
        assert!(out.content.contains("struct: Config"), "{}", out.content);
        assert!(out.content.contains("  method: new"), "{}", out.content);
        assert!(out.content.contains("fn() -> Config"), "{}", out.content);
        assert!(out.content.contains("(line 5)"), "1-based: {}", out.content);
    }

    #[test]
    fn a_long_source_line_is_clipped_rather_than_filling_the_result() {
        let long = "x".repeat(5_000);
        let clipped = clip(&long);
        assert!(
            clipped.chars().count() < MAX_LINE_CHARS + 20,
            "{}",
            clipped.len()
        );
        assert!(clipped.ends_with("[line clipped]"));
    }

    // -- completion ---------------------------------------------------------

    /// **The four fields that can decide what gets inserted, and their
    /// precedence.** Getting this wrong is not a rendering mistake: it puts the
    /// wrong characters into somebody's source file.
    #[test]
    fn a_text_edit_beats_insert_text_beats_the_label() {
        let (items, _) = parse_completions(&json!([
            // All three present. The edit wins, and its `replace` range is
            // taken over its `insert` range: a completion accepted mid-word
            // should overwrite the word, not leave the tail behind.
            {
                "label": "push(…)",
                "insertText": "push",
                "textEdit": {
                    "newText": "push",
                    "insert": { "start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 6} },
                    "replace": { "start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 9} },
                },
                "sortText": "a",
            },
            // No edit: `insertText` wins over the decorated label.
            { "label": "len(…)", "insertText": "len", "sortText": "b" },
            // Neither: the label is all there is.
            { "label": "capacity", "sortText": "c" },
        ]));
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].insert, "push");
        assert_eq!(
            items[0].replace,
            Some((
                Position {
                    line: 3,
                    character: 4
                },
                Position {
                    line: 3,
                    character: 9
                }
            )),
            "the replacing range must win over the inserting one"
        );
        assert_eq!(items[1].insert, "len");
        assert_eq!(items[1].replace, None);
        assert_eq!(items[2].insert, "capacity");
    }

    /// **What typing is matched against is `filterText`, not the label.**
    /// rust-analyzer labels a method `push(…)` and filters it as `push`; a
    /// client matching the label stops finding anything after the bracket.
    #[test]
    fn typing_is_matched_against_filter_text_and_ranked_by_sort_text() {
        let (items, incomplete) = parse_completions(&json!({
            "isIncomplete": true,
            "items": [
                // Deliberately out of order, and alphabetically the reverse of
                // the ranking the server asked for.
                { "label": "zzz_unlikely", "sortText": "zzzz" },
                { "label": "push(…)", "filterText": "push", "sortText": "aaaa" },
            ],
        }));
        assert_eq!(
            items[0].label, "push(…)",
            "the server's ranking was ignored"
        );
        assert_eq!(items[0].filter, "push");
        assert_eq!(
            items[1].filter, "zzz_unlikely",
            "an absent filterText falls back to the label"
        );
        assert!(
            incomplete,
            "isIncomplete was dropped: a client that treats a truncated list as \
             the whole answer appears to run out of suggestions as you type"
        );
    }

    /// A snippet is carried, never silently inserted. With `snippetSupport`
    /// declared false the server should not send one, and a server that does
    /// anyway must not put `${1:value}` into a file.
    #[test]
    fn a_snippet_is_flagged_rather_than_inserted_blind() {
        let (items, _) = parse_completions(&json!([
            { "label": "push", "insertText": "push(${1:value})", "insertTextFormat": 2 },
            { "label": "len", "insertText": "len()", "insertTextFormat": 1 },
        ]));
        assert!(items.iter().any(|i| i.snippet && i.insert.contains("${1:")));
        assert!(items.iter().any(|i| !i.snippet));
    }

    /// The kind is a bare integer on the wire, and an unknown one is not
    /// "text": a client that names every unknown kind is telling the reader
    /// something it does not know.
    #[test]
    fn an_unknown_completion_kind_is_left_unnamed() {
        let (items, _) = parse_completions(&json!([
            { "label": "a", "kind": 2, "sortText": "1" },
            { "label": "b", "kind": 999, "sortText": "2" },
            { "label": "c", "sortText": "3" },
        ]));
        assert_eq!(items[0].kind, "method");
        assert_eq!(items[1].kind, "", "an unknown kind was named anyway");
        assert_eq!(items[2].kind, "");
    }

    /// Both wire shapes: a bare array is a complete list, an object carries
    /// `isIncomplete`. Neither may be read as the other.
    #[test]
    fn both_completion_shapes_parse_and_only_one_can_be_incomplete() {
        let (from_array, incomplete) = parse_completions(&json!([{ "label": "x" }]));
        assert_eq!(from_array.len(), 1);
        assert!(!incomplete, "a bare array is a complete list by definition");
        let (empty, _) = parse_completions(&json!({ "items": [], "isIncomplete": false }));
        assert!(empty.is_empty());
        let (nothing, _) = parse_completions(&json!(null));
        assert!(nothing.is_empty(), "a null answer is no completions");
    }

    // -- signature help -----------------------------------------------------

    /// **A per-signature `activeParameter` beats the top-level one**, which is
    /// why the protocol added it: a server offering overloads has to say which
    /// argument is active in each, and a client reading only the outer field
    /// marks the wrong one on every overload but the first.
    #[test]
    fn the_inner_active_parameter_wins_over_the_outer_one() {
        let (sigs, active) = parse_signatures(&json!({
            "activeSignature": 1,
            "activeParameter": 0,
            "signatures": [
                { "label": "fn a(x: u8)", "parameters": [{ "label": "x: u8" }] },
                {
                    "label": "fn a(x: u8, y: u8)",
                    "parameters": [{ "label": "x: u8" }, { "label": "y: u8" }],
                    "activeParameter": 1,
                },
            ],
        }))
        .expect("signatures were offered");
        assert_eq!(active, 1);
        assert_eq!(sigs[0].active_parameter, Some(0), "the outer field applies");
        assert_eq!(sigs[1].active_parameter, Some(1), "the inner field wins");
    }

    /// A parameter label can be a pair of offsets into the signature, and those
    /// offsets are UTF-16 like every other column on this wire. Slicing by byte
    /// puts the marker on the wrong argument the moment a signature contains a
    /// character outside the basic plane.
    #[test]
    fn parameter_offsets_are_utf16_and_go_through_the_one_converter() {
        let label = "fn f(π: u8, y: u8)";
        // `π` is two bytes and one UTF-16 unit, so byte and UTF-16 offsets
        // disagree from here on: the second parameter is UTF-16 12..17 and
        // bytes 13..18. Slicing by byte would return `: u8)`.
        let (sigs, _) = parse_signatures(&json!({
            "signatures": [{
                "label": label,
                "parameters": [{ "label": [5, 10] }, { "label": [12, 17] }],
            }],
        }))
        .expect("signatures were offered");
        assert_eq!(sigs[0].parameters, vec!["π: u8", "y: u8"]);
    }

    /// An answer with no signatures is `None`, not an empty list: "the server
    /// said nothing" and "there are no signatures" must not render alike.
    #[test]
    fn an_empty_signature_answer_is_absent_rather_than_empty() {
        assert!(parse_signatures(&json!({ "signatures": [] })).is_none());
        assert!(parse_signatures(&json!(null)).is_none());
    }
}

// endregion: Tests

#[cfg(test)]
mod truncation_honesty {
    //! The class-wide truncation test lives in `tools/fs/tests/truncation.rs`
    //! and covers the fs tools only. An adversarial review found that gap by
    //! reading the LSP renderer and noticing it still used the bare
    //! `ToolOutcome::truncated()` — the form `tool-api` calls "the weaker form
    //! … never the preferred one for a new tool" — so the flag reached the
    //! model with no reason and `agent.rs` supplied its generic "this tool did
    //! not say which limit cut it" fallback. The tool knew the cap and the loss
    //! the whole time.
    //!
    //! This module is the LSP half of that guard, kept beside the code rather
    //! than in the fs crate's test file, because a cross-crate test would have
    //! to build a language server to reach these functions.

    use super::*;

    /// Neither renderer may report a cut without saying which cap and what to
    /// do — asserted on the **outcome**, not on the source text.
    ///
    /// **The source grep this replaces could not fail for the mutation that
    /// matters.** It asserted `!source.contains("outcome.truncated()")`, one
    /// literal spelling. An adversarial reviewer wrote the same removal as
    /// `ToolOutcome::truncated(outcome)` — UFCS, which `cargo fmt` leaves
    /// alone — and `fmt`, `clippy` and this test were all green while a symbols
    /// response reached the model marked truncated with no cap, no loss and no
    /// remedy. (An earlier attempt spelled `outcome\n.truncated()` and *was*
    /// caught, by `fmt` collapsing the chain, which is luck rather than a
    /// guard.) It was also scoped to `include_str!("render.rs")`, so a third
    /// renderer in any other file of this crate was outside it entirely.
    ///
    /// This is the `DEF-018` scar — a source assertion standing in for a
    /// behavioural one — repeated one file from where `DEF-018` fixed it.
    ///
    /// Building the capped outcome and reading `truncation` costs no more and
    /// survives every spelling, every refactor, and a renderer that has not
    /// been written yet, as long as it is called here.
    #[test]
    fn neither_renderer_reports_a_cut_without_naming_the_cap_and_the_remedy() {
        fn assert_honest(out: &ToolOutcome, which: &str) {
            assert!(out.truncated, "{which}: the cut was not flagged at all");
            let reason = out
                .truncation
                .as_deref()
                .unwrap_or_else(|| panic!("{which}: flagged truncated with no reason at all"));
            assert!(
                reason.chars().any(|c| c.is_ascii_digit()),
                "{which}: the reason names no cap: {reason}"
            );
            // The remedy. `tool-api`'s rule is that a cut names the cap, the
            // loss AND what to do — or says plainly that nothing raises it.
            assert!(
                reason.contains("No argument raises"),
                "{which}: the reason offers no remedy and does not say there is none: {reason}"
            );
        }

        // The same shape the sibling tests use; duplicated rather than made
        // public, because a fixture escaping its test module is a wider change
        // than this test is worth.
        let server = Server {
            program: std::path::PathBuf::from("/usr/bin/a-language-server"),
            args: Vec::new(),
            entry: std::path::PathBuf::from("/usr/bin/a-language-server"),
            version: "0.3.0".into(),
            source: crate::server::Source::Path,
            language: crate::lang::by_key("rust").expect("rust is in the table"),
        };
        let readiness = Readiness::Ready;

        // Symbols, over the cap.
        let items: Vec<Value> = (0..MAX_SYMBOLS + 25)
            .map(|i| serde_json::json!({ "name": format!("sym{i}"), "kind": 12 }))
            .collect();
        let out = symbols(&server, readiness, None, &Value::Array(items));
        assert_honest(&out, "symbols");

        // Locations, over the cap. They are contained against a root, so the
        // paths have to be inside one that exists.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let found: Vec<Location> = (0..MAX_LOCATIONS + 25)
            .map(|i| Location {
                path: root.join(format!("f{i}.rs")),
                line: 1,
                character: 1,
            })
            .collect();
        let out = locations(&root, &server, readiness, None, "references", found);
        assert_honest(&out, "locations");

        // The positive control, and it is doing real work: a renderer that
        // flagged everything truncated would satisfy every assertion above.
        let out = symbols(
            &server,
            readiness,
            None,
            &serde_json::json!([{ "name": "only", "kind": 12 }]),
        );
        assert!(
            !out.truncated,
            "a result that fitted was reported as cut: {:?}",
            out.truncation
        );
    }
}

// region: Completion
// ---------------------------------------------------------------------------
// Completion
//
// The defining feature of what people call intellisense, and the one shape in
// this file where the wire is genuinely more complicated than it looks. Four
// separate fields can decide what text a chosen item inserts, they disagree
// with each other on purpose, and picking the wrong one is not a rendering
// mistake: it puts the wrong characters into somebody's source file.
//
// The precedence, which is the protocol's and not a preference:
//
//   1. `textEdit` (or `insertReplaceEdit`) wins outright. It carries its own
//      range, so the server is saying *replace exactly this*, which is how
//      `.` completions replace a partial word rather than doubling it.
//   2. `insertText` next: the same idea without a range, replacing whatever
//      the client decides the current word is.
//   3. `label` last, and only because something must be.
//
// `filterText` is what the *client* matches typing against, and it is not the
// label: rust-analyzer sends labels like `push(…)` while the thing a person is
// typing against is `push`. Matching the label is why some clients feel like
// they stop finding anything after the second character.
// ---------------------------------------------------------------------------

/// What a completion item inserts, and what it is matched against.
///
/// Deliberately not the whole `CompletionItem`: this crate keeps what a person
/// or a model can act on and drops the rest, so a field here is one somebody
/// reads. `snippet` is carried rather than expanded, because expanding one
/// needs a cursor the crate does not own; the consumer decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// What the list shows.
    pub label: String,
    /// What typing is matched against. `filterText` when the server sent one,
    /// otherwise the label, per the protocol.
    pub filter: String,
    /// The text to put in the document.
    pub insert: String,
    /// The range `insert` replaces, when the server named one. `None` means the
    /// consumer decides, which for a text editor is the word before the cursor.
    pub replace: Option<(Position, Position)>,
    /// The protocol's `CompletionItemKind`, as a word. Empty when absent rather
    /// than guessed: an item of unknown kind is not a "Text" item.
    pub kind: &'static str,
    /// The one-line hint beside the label: a type, a signature, a module path.
    pub detail: Option<String>,
    /// The long form, if the server sent one without being asked.
    pub documentation: Option<String>,
    /// The server's own ordering key. Servers rank far better than an
    /// alphabetical sort does, and rust-analyzer in particular encodes
    /// relevance here, so this is sorted on and the label is not.
    pub sort: String,
    /// Whether the text is a snippet with placeholders (`${1:x}`) rather than
    /// literal characters. Carried so a consumer that cannot expand one can say
    /// so rather than inserting the braces.
    pub snippet: bool,
}

/// `CompletionItemKind`, which is a bare integer on the wire.
///
/// The empty string for an unknown number rather than a guess: a client that
/// renders every unknown kind as "Text" is telling the reader something it does
/// not know.
fn completion_kind(n: u64) -> &'static str {
    match n {
        1 => "text",
        2 => "method",
        3 => "function",
        4 => "constructor",
        5 => "field",
        6 => "variable",
        7 => "class",
        8 => "interface",
        9 => "module",
        10 => "property",
        11 => "unit",
        12 => "value",
        13 => "enum",
        14 => "keyword",
        15 => "snippet",
        16 => "color",
        17 => "file",
        18 => "reference",
        19 => "folder",
        20 => "enum member",
        21 => "constant",
        22 => "struct",
        23 => "event",
        24 => "operator",
        25 => "type parameter",
        _ => "",
    }
}

/// One `CompletionItem`, or `None` when it carries nothing to insert.
fn parse_completion(v: &Value) -> Option<Completion> {
    let label = v.get("label").and_then(Value::as_str)?.to_string();
    // `insertTextFormat`: 2 is a snippet, 1 or absent is plain text.
    let snippet = v
        .get("insertTextFormat")
        .and_then(Value::as_u64)
        .is_some_and(|f| f == 2);

    // The precedence in the region header, in order.
    let edit = v.get("textEdit");
    let (insert, replace) = match edit {
        Some(e) => {
            let text = e
                .get("newText")
                .and_then(Value::as_str)
                .unwrap_or(&label)
                .to_string();
            // An `InsertReplaceEdit` carries two ranges. `replace` is the one
            // that overwrites what is already there, which is what a completion
            // accepted mid-word should do; `insert` would leave the tail.
            let range = e
                .get("replace")
                .or_else(|| e.get("insert"))
                .or_else(|| e.get("range"));
            (text, range.and_then(parse_range))
        }
        None => (
            v.get("insertText")
                .and_then(Value::as_str)
                .unwrap_or(&label)
                .to_string(),
            None,
        ),
    };

    Some(Completion {
        filter: v
            .get("filterText")
            .and_then(Value::as_str)
            .unwrap_or(&label)
            .to_string(),
        sort: v
            .get("sortText")
            .and_then(Value::as_str)
            .unwrap_or(&label)
            .to_string(),
        kind: v
            .get("kind")
            .and_then(Value::as_u64)
            .map(completion_kind)
            .unwrap_or(""),
        detail: v
            .get("detail")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|d| !d.trim().is_empty()),
        documentation: v.get("documentation").and_then(documentation_text),
        label,
        insert,
        replace,
        snippet,
    })
}

/// A range's two ends, as positions.
fn parse_range(r: &Value) -> Option<(Position, Position)> {
    let one = |k: &str| -> Option<Position> {
        let p = r.get(k)?;
        Some(Position {
            line: p.get("line")?.as_u64()? as u32,
            character: p.get("character")?.as_u64()? as u32,
        })
    };
    Some((one("start")?, one("end")?))
}

/// `Documentation` is a string or a `MarkupContent`, like hover's `contents`.
fn documentation_text(v: &Value) -> Option<String> {
    let text = match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => o.get("value").and_then(Value::as_str)?.to_string(),
        _ => return None,
    };
    Some(text).filter(|t| !t.trim().is_empty())
}

/// Every completion the server offered, best first, and whether the list is
/// partial.
///
/// **`isIncomplete` is carried rather than hidden**, and it is the one thing a
/// consumer must not ignore: a `true` means the server truncated its own answer
/// for speed and expects to be asked again as the person types. A client that
/// treats an incomplete list as the whole answer filters a shrinking set and
/// appears to run out of suggestions.
///
/// Sorted by `sortText`, then by label to break ties deterministically. The
/// server's ranking is far better than alphabetical, and this is where that is
/// respected rather than in the consumer.
pub fn parse_completions(value: &Value) -> (Vec<Completion>, bool) {
    let (items, incomplete) = match value {
        Value::Array(items) => (items.clone(), false),
        Value::Object(o) => (
            o.get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            o.get("isIncomplete")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        _ => (Vec::new(), false),
    };
    let mut out: Vec<Completion> = items.iter().filter_map(parse_completion).collect();
    out.sort_by(|a, b| a.sort.cmp(&b.sort).then_with(|| a.label.cmp(&b.label)));
    (out, incomplete)
}

// endregion: Completion

// region: Signature help
// ---------------------------------------------------------------------------
// Signature help
//
// The other half of typing help, and much simpler than completion with one
// exception: `activeParameter` can arrive in two places and the inner one wins.
// The protocol added a per-signature `activeParameter` after the top-level one,
// precisely because a server offering several overloads needs to say which
// argument is active *in each*. A client reading only the outer field marks the
// wrong argument on every overload but the first.
// ---------------------------------------------------------------------------

/// One callable's signature, with the argument the cursor is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The whole signature as the server writes it.
    pub label: String,
    /// Each parameter's label, in order.
    pub parameters: Vec<String>,
    /// Which parameter the cursor is in, when the server said. `None` rather
    /// than zero, because zero is a real answer meaning the first argument.
    pub active_parameter: Option<usize>,
    pub documentation: Option<String>,
}

/// Every signature offered, and which of them is the active one.
///
/// Returns `None` for the empty answer rather than an empty list, so a caller
/// cannot render "no signatures" and a server that said nothing the same way.
pub fn parse_signatures(value: &Value) -> Option<(Vec<Signature>, usize)> {
    let sigs = value.get("signatures")?.as_array()?;
    if sigs.is_empty() {
        return None;
    }
    let outer = value.get("activeParameter").and_then(Value::as_u64);
    let active = value
        .get("activeSignature")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let out: Vec<Signature> = sigs
        .iter()
        .map(|sig| {
            let parameters = sig
                .get("parameters")
                .and_then(Value::as_array)
                .map(|ps| {
                    ps.iter()
                        .filter_map(|p| match p.get("label") {
                            // A parameter label is a string, or a pair of
                            // offsets into the signature label. The offsets are
                            // in UTF-16 units like every other column on this
                            // wire, so they go through `doc` rather than being
                            // sliced by byte.
                            Some(Value::String(s)) => Some(s.clone()),
                            Some(Value::Array(pair)) => {
                                let label = sig.get("label")?.as_str()?;
                                let a = pair.first()?.as_u64()? as u32;
                                let b = pair.get(1)?.as_u64()? as u32;
                                let (from, to) = (
                                    crate::doc::byte_offset(label, a),
                                    crate::doc::byte_offset(label, b),
                                );
                                label.get(from..to).map(str::to_string)
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            Signature {
                label: sig
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                parameters,
                // The inner field wins where it exists; see the region header.
                active_parameter: sig
                    .get("activeParameter")
                    .and_then(Value::as_u64)
                    .or(outer)
                    .map(|n| n as usize),
                documentation: sig.get("documentation").and_then(documentation_text),
            }
        })
        .collect();
    Some((out, active.min(sigs.len().saturating_sub(1))))
}

// endregion: Signature help
