//! A chromehand digest, rendered as markdown.
//!
//! **Why not just hand the model the JSON?** Because the digest is shaped for
//! a machine that will index into it, and a model reading a page is doing
//! prose comprehension. Handing over `{"digest":{"text":"…"}}` spends tokens
//! on punctuation and structure the model must undo before it can read the
//! sentence, and it invites the model to quote JSON back at the user.
//!
//! **The split, which is the actual design decision here.** chromehand returns
//! two different things: what the page *says*, and what the page *offers to
//! do* — an inventory of every link, button, field and form with a stable
//! selector for each. Those serve different questions:
//!
//! - To *answer a question about* a page, a model needs the prose, the tables,
//!   the structured data, and the links (a link is where the answer continues).
//! - To *act on* a page it needs selectors, field names, option lists, which
//!   control submits.
//!
//! This pass ships reading only — the interaction verbs exist in
//! `crate::chromehand` and are not exposed as tools. So the inventory is
//! rendered as counts and form shapes rather than sixty selectors: enough for
//! the model to report "there is a search form here" and enough for a human to
//! know acting is possible, without spending a thousand tokens on addresses
//! for a thing nothing can address yet. When the verbs are exposed, the
//! selectors come back — and they should come back in the *action* tool's
//! output, not in every page read.
//!
//! **Truncation is passed through, never absorbed.** chromehand caps its own
//! text and says so with `text_truncated`; this renderer caps the link list
//! and the JSON-LD. Either one sets `ToolOutcome::truncated` and says so in
//! the prose, because silent truncation is indistinguishable from a short page
//! and the model will reason confidently about the part it never saw.

use serde_json::Value;

// region: What gets cut, and how much
// ---------------------------------------------------------------------------
// What gets cut, and how much
//
// Two budgets and the struct that reports them. Everything cut is announced —
// `truncated` is true if anything was dropped anywhere, chromehand's own text
// cap included, because silent truncation is indistinguishable from a short
// page and the model will reason confidently about what it never saw.
// ---------------------------------------------------------------------------

/// Links are the part of the inventory that survives, and this is where the
/// budget goes. Fifty is roughly a page of navigation plus its content links.
/// chromehand's own in-page cap is higher (120), so this is the second of two
/// budgets and the one that usually binds.
const MAX_LINKS: usize = 50;
/// JSON-LD is often the densest true statement on a page (a job posting, a
/// product, an article's byline) and often a marketing blob. Capped, not cut.
const MAX_JSON_LD_CHARS: usize = 2_000;

pub struct Rendered {
    pub markdown: String,
    /// True when *anything* was cut — chromehand's own text cap included.
    pub truncated: bool,
    /// The one-line terminal note. Never the page.
    pub display: String,
}

// endregion: What gets cut, and how much

// region: The page, in reading order
// ---------------------------------------------------------------------------
// The page, in reading order
//
// One long function, deliberately, because the order of the sections IS the
// design: provenance, then a block-page warning if there is one, then the
// prose, then structure, links, and what the page offers to do. Splitting it
// into a renderer per section would hide the sequencing, which is the part
// that had to be got right.
// ---------------------------------------------------------------------------

/// Render a `digest` output object.
///
/// `Err` here means the JSON was not a digest at all — a contract breach worth
/// surfacing as a failure. Every honest negative (no text, blocked, a rendered
/// 404) renders successfully and says so in the prose.
///
/// The one non-obvious consequence: this renderer requires the `digest` key,
/// which [`crate::chromehand::verify_url`] deliberately omits. A verify result
/// is not renderable here and is not meant to be — `WebFetch` calls
/// `digest_url` and nothing else. Pointing this at a verify output would read
/// as a browser fault when the truth is that the wrong command was run.
pub fn render(v: &Value) -> Result<Rendered, String> {
    let digest = v
        .get("digest")
        .and_then(Value::as_object)
        .ok_or_else(|| "the browser returned no digest payload".to_string())?;

    let mut out = String::new();
    let mut truncated = false;

    // ---------------------------------------------------------- provenance --
    let title = str_at(v, "page_title")
        .or_else(|| digest.get("meta").and_then(|m| str_at(m, "title")))
        .unwrap_or("(untitled page)");
    out.push_str(&format!("# {}\n\n", one_line(title)));

    let requested = str_at(v, "url").unwrap_or("");
    let final_url = str_at(v, "final_url").unwrap_or(requested);
    out.push_str(&format!("- URL: {final_url}\n"));
    if !requested.is_empty() && requested != final_url {
        out.push_str(&format!("- Redirected from: {requested}\n"));
    }
    out.push_str(&format!("- Status: {}\n", status_line(v)));
    if let Some(ts) = v.get("evidence").and_then(|e| str_at(e, "fetch_timestamp")) {
        out.push_str(&format!("- Read at: {ts}\n"));
    }
    out.push('\n');

    // An anti-bot challenge is a result, not a failure — and it is the single
    // most important thing on the page, so it goes above the prose rather than
    // in a footnote the model may skim past.
    if v.get("looks_blocked").and_then(Value::as_bool) == Some(true) {
        out.push_str(
            "**This page served an anti-bot challenge rather than its content.** What follows \
             is the challenge, not the page. This is the honest answer and it is not worked \
             around; if the content is needed, it needs a human or a different source.\n\n",
        );
    }

    if let Some(desc) = digest.get("meta").and_then(|m| str_at(m, "description")) {
        if !desc.trim().is_empty() {
            out.push_str(&format!("> {}\n\n", one_line(desc)));
        }
    }

    // -------------------------------------------------------------- content --
    out.push_str("## Content\n\n");
    let text = digest.get("text").and_then(Value::as_str).unwrap_or("");
    if text.trim().is_empty() {
        // The governing rule, made visible: a page with nothing on it is a
        // result. This sentence is the difference between the model looking
        // elsewhere and the model retrying a fetch that will never differ.
        out.push_str(
            "(the page rendered, and its main content held no readable text — it may be an \
             image, a video, an app shell, or a redirect stub)\n",
        );
    } else {
        out.push_str(text.trim_end());
        out.push('\n');
    }
    if digest.get("text_truncated").and_then(Value::as_bool) == Some(true) {
        truncated = true;
        let total = digest
            .get("text_chars_total")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        out.push_str(&format!(
            "\n[truncated: {} of {total} characters shown. Raise max_chars to see more.]\n",
            text.chars().count()
        ));
    }
    out.push('\n');

    // ----------------------------------------------------------- structured --
    if let Some(structured) = digest.get("structured") {
        let tables = structured
            .get("tables")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for (i, table) in tables.iter().enumerate() {
            if let Some(md) = render_table(table) {
                out.push_str(&format!("## Table {}\n\n{md}\n", i + 1));
            }
        }
        let ld = structured
            .get("json_ld")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if !ld.is_empty() {
            let body = serde_json::to_string_pretty(ld).unwrap_or_default();
            out.push_str("## Structured data (JSON-LD)\n\n```json\n");
            if body.chars().count() > MAX_JSON_LD_CHARS {
                truncated = true;
                out.push_str(&take_chars(&body, MAX_JSON_LD_CHARS));
                out.push_str("\n… [truncated: JSON-LD longer than ");
                out.push_str(&MAX_JSON_LD_CHARS.to_string());
                out.push_str(" characters]");
            } else {
                out.push_str(&body);
            }
            out.push_str("\n```\n\n");
        }
    }

    // ---------------------------------------------------------------- links --
    let interactive = digest.get("interactive");
    let links = interactive
        .and_then(|i| i.get("links"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !links.is_empty() {
        // Main-content links first: on a real page the nav is longer than the
        // article, and the article's links are the ones that answer anything.
        let mut ordered: Vec<&Value> = links.iter().filter(|l| in_content(l)).collect();
        ordered.extend(links.iter().filter(|l| !in_content(l)));

        out.push_str(&format!("## Links ({})\n\n", links.len()));
        for link in ordered.iter().take(MAX_LINKS) {
            let text = str_at(link, "text").unwrap_or("");
            let href = str_at(link, "href").unwrap_or("");
            let label = if text.trim().is_empty() {
                href
            } else {
                text.trim()
            };
            out.push_str(&format!("- [{}]({href})\n", link_label(label)));
        }
        if ordered.len() > MAX_LINKS {
            truncated = true;
            out.push_str(&format!(
                "\n[truncated: {} of {} links shown, main-content links first]\n",
                MAX_LINKS,
                ordered.len()
            ));
        }
        out.push('\n');
    }

    // ---------------------------------------------------- what could be done --
    if let Some(i) = interactive {
        let forms = arr_len(i, "forms");
        let fields = arr_len(i, "fields");
        let buttons = arr_len(i, "buttons");
        if forms + fields + buttons > 0 {
            out.push_str("## Interactive elements\n\n");
            out.push_str(&format!(
                "{}, {}, {}.\n",
                plural(forms, "form"),
                plural(fields, "form field"),
                plural(buttons, "button")
            ));
            for form in i
                .get("forms")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let method = str_at(form, "method").unwrap_or("get").to_uppercase();
                let action = str_at(form, "action").unwrap_or("(same page)");
                let count = form
                    .get("field_count")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                out.push_str(&format!("- {method} {action} — {count} fields\n"));
            }
            out.push_str(
                "\nSelectors are not listed: this tool reads pages, it does not act on them. \
                 Nothing here can be clicked, typed into, or submitted.\n\n",
            );
        }
    }

    // Cross-origin frames are content that exists and was not read. Saying so
    // is the difference between an incomplete answer and a wrong one.
    let unreadable = digest
        .get("frames_unreadable")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !unreadable.is_empty() {
        out.push_str(&format!(
            "## Not read\n\n{} on this page are cross-origin iframes whose content could not \
             be read. Their sources:\n\n",
            plural(unreadable.len(), "region")
        ));
        for frame in unreadable {
            out.push_str(&format!(
                "- {}\n",
                str_at(frame, "src").unwrap_or("(no src)")
            ));
        }
        out.push('\n');
    }

    let display = format!(
        "{} — {} ({} chars, {} links)",
        one_line(title),
        status_line(v),
        text.chars().count(),
        links.len()
    );

    Ok(Rendered {
        markdown: out,
        truncated,
        display,
    })
}

// endregion: The page, in reading order

// region: Rendering text a hostile page wrote
// ---------------------------------------------------------------------------
// Rendering text a hostile page wrote
//
// Titles, link labels and table cells are all authored by the page, and all of
// them land inside markdown that has its own syntax. The consistent choice
// here is to escape the character that would break the line rather than drop
// it — a pipe or a bracket is frequently the data.
// ---------------------------------------------------------------------------

fn status_line(v: &Value) -> String {
    let outcome = str_at(v, "outcome").unwrap_or("unknown");
    match v.get("http_status").and_then(Value::as_i64) {
        Some(code) => format!("HTTP {code}, {outcome}"),
        // `null` means CDP never observed a main-document response. chromehand
        // refuses to guess one and so does this.
        None => format!("HTTP status not observed, {outcome}"),
    }
}

fn in_content(link: &Value) -> bool {
    link.get("in_content").and_then(Value::as_bool) == Some(true)
}

fn arr_len(v: &Value, key: &str) -> usize {
    v.get(key).and_then(Value::as_array).map_or(0, Vec::len)
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Link text is page-authored and may contain markdown that would break the
/// line it is placed in. Collapsed to one line and the two delimiters escaped;
/// nothing else, because mangling the words costs more than a stray asterisk.
fn link_label(s: &str) -> String {
    let flat = one_line(s).replace('[', "\\[").replace(']', "\\]");
    if flat.chars().count() > 120 {
        format!("{}…", take_chars(&flat, 119))
    } else {
        flat
    }
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn render_table(table: &Value) -> Option<String> {
    let rows = table.as_array()?;
    let (header, body) = rows.split_first()?;
    let header: Vec<String> = header.as_array()?.iter().map(cell).collect();
    if header.is_empty() {
        return None;
    }
    let mut md = format!("| {} |\n", header.join(" | "));
    md.push_str(&format!("| {} |\n", vec!["---"; header.len()].join(" | ")));
    for row in body {
        let mut cells: Vec<String> = row
            .as_array()
            .map(|r| r.iter().map(cell).collect())
            .unwrap_or_default();
        cells.resize(header.len(), String::new());
        md.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    Some(md)
}

fn cell(v: &Value) -> String {
    // A literal pipe in a cell ends the cell. Escaped rather than dropped: the
    // character is often the data (a path, a shell snippet, a units column).
    one_line(v.as_str().unwrap_or("")).replace('|', "\\|")
}

// endregion: Rendering text a hostile page wrote

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// Every one of these defends a case where the wrong output is plausible rather
// than obviously broken: an empty page reading as a complaint, a cut that was
// not announced, a warning buried below the prose it warns about, a status
// invented because none was observed.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn digest_with(text: &str, extra: Value) -> Value {
        let mut d = json!({
            "command": "digest",
            "url": "https://example.com",
            "final_url": "https://example.com/",
            "http_status": 200,
            "page_title": "Example Domain",
            "looks_blocked": false,
            "outcome": "verified",
            "evidence": { "source": "browser-render", "fetch_timestamp": "2026-08-09T00:00:00Z" },
            "digest": {
                "text": text,
                "text_chars_total": text.chars().count(),
                "text_truncated": false,
                "interactive": { "forms": [], "fields": [], "buttons": [], "links": [] }
            }
        });
        merge(&mut d, extra);
        d
    }

    fn merge(base: &mut Value, extra: Value) {
        for (k, v) in extra.as_object().unwrap() {
            match (base.get_mut(k), v) {
                (Some(existing), Value::Object(_)) if existing.is_object() => {
                    merge(existing, v.clone())
                }
                _ => {
                    base[k] = v.clone();
                }
            }
        }
    }

    #[test]
    fn an_empty_page_renders_as_a_result_not_a_complaint() {
        // The governing rule. If this ever renders as an error, "I could not
        // look" and "I looked and it was empty" have become the same message.
        let r = render(&digest_with("", json!({}))).expect("empty text is renderable");
        assert!(!r.truncated);
        assert!(r.markdown.contains("no readable text"), "{}", r.markdown);
        assert!(r.markdown.contains("HTTP 200"), "{}", r.markdown);
    }

    #[test]
    fn chromehands_own_truncation_is_passed_through() {
        let v = digest_with(
            "half a page",
            json!({ "digest": { "text_truncated": true, "text_chars_total": 90_000 } }),
        );
        let r = render(&v).unwrap();
        assert!(r.truncated, "chromehand cut the text and we reported whole");
        assert!(r.markdown.contains("90000"), "{}", r.markdown);
    }

    #[test]
    fn a_blocked_page_says_so_before_the_prose() {
        let v = digest_with(
            "Verify you are human",
            json!({ "looks_blocked": true, "outcome": "blocked" }),
        );
        let r = render(&v).unwrap();
        let warning = r.markdown.find("anti-bot challenge").expect("no warning");
        let content = r.markdown.find("## Content").expect("no content heading");
        assert!(warning < content, "the warning must precede the prose");
    }

    #[test]
    fn main_content_links_come_first_and_the_list_is_capped() {
        let mut links: Vec<Value> = (0..MAX_LINKS + 10)
            .map(|i| json!({ "text": format!("chrome {i}"), "href": "https://x/", "in_content": false }))
            .collect();
        links.push(json!({ "text": "the answer", "href": "https://a/", "in_content": true }));
        let v = digest_with(
            "body",
            json!({ "digest": { "interactive": { "links": links } } }),
        );
        let r = render(&v).unwrap();
        assert!(r.truncated, "links were cut without saying so");
        let first = r
            .markdown
            .find("the answer")
            .expect("in-content link dropped");
        let chrome = r.markdown.find("chrome 0").expect("chrome link dropped");
        assert!(first < chrome, "page chrome outranked the content");
    }

    #[test]
    fn an_unobserved_status_is_not_invented() {
        let v = digest_with(
            "body",
            json!({ "http_status": Value::Null, "outcome": "failed" }),
        );
        let r = render(&v).unwrap();
        assert!(r.markdown.contains("not observed"), "{}", r.markdown);
    }

    #[test]
    fn a_response_that_is_not_a_digest_is_a_failure() {
        let r = render(&json!({ "error": "browser", "detail": "nope" }));
        assert!(r.is_err(), "a non-digest rendered as a page");
    }
}

// endregion: Tests
