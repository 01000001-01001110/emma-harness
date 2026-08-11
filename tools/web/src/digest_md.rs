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
//! Which of those two a call needs is [`Limits::show_selectors`], and the
//! default is off. `WebFetch` reads one page and cannot act on it, so listing
//! sixty CSS selectors there would spend a thousand tokens on addresses for a
//! thing that call cannot address: it gets counts and form shapes, enough to
//! report "there is a search form here". `BrowserRead` is a read *inside a
//! session that can click*, which is already an act of intent to interact, so it
//! turns them on and can afford them.
//!
//! That split is the instruction this file left for itself when the interaction
//! verbs were still unexposed — "when the verbs are exposed, the selectors come
//! back, and they should come back in the *action* tool's output, not in every
//! page read" — followed as written.
//!
//! **Truncation is passed through, never absorbed.** chromehand caps its own
//! text and says so with `text_truncated`; this renderer caps the link list
//! and the JSON-LD. Either one sets `ToolOutcome::truncated` and says so in
//! the prose, because silent truncation is indistinguishable from a short page
//! and the model will reason confidently about the part it never saw.
//!
//! **And "passed through" means the numbers, not the word.** A notice that
//! says only *that* something was cut is the same failure one step milder: the
//! reader cannot tell whether the article or the navigation went missing, how
//! much of it there was, or which argument would bring it back. Every cut here
//! names the cap that bound, the amount dropped, and the way to get the rest —
//! or says plainly that there is no way, which is also an answer. Those lines
//! are collected in [`Rendered::truncation`] so the same sentence reaches the
//! model in the result and the human on the terminal.

use serde_json::Value;

// region: What gets cut, and how much
// ---------------------------------------------------------------------------
// What gets cut, and how much
//
// Two budgets and the struct that reports them. Everything cut is announced —
// `truncation` is `Some` if anything was dropped anywhere, chromehand's own
// text cap included, because silent truncation is indistinguishable from a
// short page and the model will reason confidently about what it never saw.
// ---------------------------------------------------------------------------

/// Links are the part of the inventory that survives, so this is where the
/// caller's budget goes. Fifty is roughly a page of navigation plus its
/// content links, which is right for an article and wrong for a hub — on a
/// news index the links *are* the content. Hence a caller-supplied number
/// rather than a constant; the default stays 50 and [`crate::fetch`] owns it.
///
/// chromehand's own in-page collector stops at 120, so a budget above that
/// cannot produce more links: there are none to produce. Saying so is the
/// difference between a raisable cap and a promise that quietly fails.
pub const COLLECTOR_LINK_BUDGET: usize = 120;

/// JSON-LD is often the densest true statement on a page (a job posting, a
/// product, an article's byline) and often a marketing blob. Capped, not cut —
/// and unlike the other two this cap is not raisable, which the notice says
/// rather than implying a knob that does not exist.
const MAX_JSON_LD_CHARS: usize = 2_000;

/// The budgets this renderer applies, supplied per call.
///
/// They live here rather than as constants because the caller is the only one
/// that knows what the model asked for, and a cap the model cannot move is a
/// cap it cannot be advised to move.
#[derive(Debug, Clone)]
pub struct Limits {
    /// How many links to list. Above [`COLLECTOR_LINK_BUDGET`] nothing more
    /// exists to list.
    pub max_links: usize,
    /// What the caller passed as `max_chars`, used only to name the argument
    /// accurately when chromehand's text cap is the one that bound.
    pub max_chars: usize,
    /// List each interactive element's stable CSS selector.
    ///
    /// Off for `WebFetch`, which cannot act on what it reads; on for
    /// `BrowserRead`, which is a read inside a session that can. See the module
    /// doc — this is the one knob that decides which of the digest's two halves
    /// a call is paying for.
    pub show_selectors: bool,
    /// Case-insensitive substring the listed elements must match, against their
    /// selector, their label or their text.
    ///
    /// A page with a hundred addressable elements costs more than a model
    /// looking for the search box needs to pay. Ignored when
    /// [`Limits::show_selectors`] is off, because there is then nothing to
    /// filter.
    pub selector_filter: Option<String>,
}

impl Limits {
    /// The reading defaults: no selectors, no filter.
    pub fn reading(max_links: usize, max_chars: usize) -> Self {
        Self {
            max_links,
            max_chars,
            show_selectors: false,
            selector_filter: None,
        }
    }
}

/// How many addressable elements one read lists per category.
///
/// A selector line is short but a hundred of them is a page of noise, and the
/// filter is the intended remedy — so the cut says so rather than advertising a
/// number to raise.
pub const SELECTOR_BUDGET: usize = 40;

pub struct Rendered {
    pub markdown: String,
    /// Every cut that happened, one line each, already carrying its numbers
    /// and its remedy. `None` means nothing was dropped anywhere — chromehand's
    /// own text cap included.
    pub truncation: Option<String>,
    /// The one-line terminal note. Never the page.
    pub display: String,
}

impl Rendered {
    /// True when *anything* was cut.
    pub fn truncated(&self) -> bool {
        self.truncation.is_some()
    }
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
pub fn render(v: &Value, limits: &Limits) -> Result<Rendered, String> {
    let digest = v
        .get("digest")
        .and_then(Value::as_object)
        .ok_or_else(|| "the browser returned no digest payload".to_string())?;

    let mut out = String::new();
    // One entry per cap that bound, each already a complete sentence with its
    // numbers and its remedy. Kept as a list rather than a bool so a page that
    // lost both its prose and its links does not report only the first.
    let mut cuts: Vec<String> = Vec::new();

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
        let shown = text.chars().count();
        let total = digest
            .get("text_chars_total")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        // The one cut that costs prose, so it names the argument, the value
        // that was in force, and what to set it to — "raise max_chars" alone
        // leaves the reader guessing what it currently is.
        let cut = format!(
            "page text cut to {shown} of {total} characters by max_chars={}; \
             re-read with max_chars={total} for the whole page",
            limits.max_chars
        );
        out.push_str(&format!("\n[truncated: {cut}]\n"));
        cuts.push(cut);
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
            let total = body.chars().count();
            if total > MAX_JSON_LD_CHARS {
                // No argument raises this one, and pretending otherwise would
                // send the model to spend a turn on a knob that does not
                // exist. The honest remedy is the page's own prose and links,
                // which are already above.
                let cut = format!(
                    "JSON-LD cut to {MAX_JSON_LD_CHARS} of {total} characters by a fixed cap \
                     no argument raises; the page's text and links above are the whole of what \
                     this tool can return"
                );
                out.push_str(&take_chars(&body, MAX_JSON_LD_CHARS));
                out.push_str(&format!("\n… [truncated: {cut}]"));
                cuts.push(cut);
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
        for link in ordered.iter().take(limits.max_links) {
            let text = str_at(link, "text").unwrap_or("");
            let href = str_at(link, "href").unwrap_or("");
            let label = if text.trim().is_empty() {
                href
            } else {
                text.trim()
            };
            // The selector rides along on a session read, because a link is the
            // single most clickable thing on a page and `BrowserAct` addresses
            // by selector, not by href — clicking is how a link with a JS
            // handler, or one inside a single-page app, actually works.
            match (limits.show_selectors, str_at(link, "selector")) {
                (true, Some(selector)) => out.push_str(&format!(
                    "- [{}]({href}) — `{}`\n",
                    link_label(label),
                    one_line(selector)
                )),
                _ => out.push_str(&format!("- [{}]({href})\n", link_label(label))),
            }
        }
        if ordered.len() > limits.max_links {
            let total = ordered.len();
            // On a hub page this is the cut that loses the answer, and it is
            // the one that used to be unattributable: the page text was well
            // inside its cap, so a bare "truncated" pointed the reader at
            // max_chars, which would have changed nothing. Name the right
            // argument, and name the ceiling above which raising it is futile
            // because the collector stopped there.
            let ceiling = if total >= COLLECTOR_LINK_BUDGET {
                format!(
                    " — {COLLECTOR_LINK_BUDGET} is the most this browser collects from one page, \
                     so links beyond that were never captured"
                )
            } else {
                String::new()
            };
            let cut = format!(
                "{} of {total} links shown (main-content links first), {} dropped by \
                 max_links={}; re-read with max_links={total} for the rest{ceiling}",
                limits.max_links,
                total - limits.max_links,
                limits.max_links
            );
            out.push_str(&format!("\n[truncated: {cut}]\n"));
            cuts.push(cut);
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
            if limits.show_selectors {
                // The addresses. Only reached from a session read, and the whole
                // reason `BrowserAct` can hit anything: every element carries the
                // stable selector chromehand computed for it in-page.
                out.push('\n');
                for (key, heading) in [("fields", "Fields"), ("buttons", "Buttons")] {
                    let items = i
                        .get(key)
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or_default();
                    if let Some(section) =
                        render_addressable(items, heading, limits.selector_filter.as_deref())
                    {
                        out.push_str(&section.body);
                        if let Some(cut) = section.cut {
                            cuts.push(cut);
                        }
                    }
                }
            } else {
                out.push_str(
                    "\nSelectors are not listed: this tool reads pages, it does not act on \
                     them. Nothing here can be clicked, typed into, or submitted. To act on \
                     this page, open a browser session with `BrowserOpen` — a session read \
                     lists the selectors `BrowserAct` needs.\n\n",
                );
            }
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
        // Joined rather than merged: two caps that bound are two facts, and a
        // summary that says "output was truncated" for both is how the reader
        // ends up believing the prose was cut when only the link list was.
        truncation: (!cuts.is_empty()).then(|| cuts.join("; also ")),
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

/// One rendered `## Fields` / `## Buttons` block, and whatever it had to drop.
struct Addressable {
    body: String,
    cut: Option<String>,
}

/// The addressable elements of one category, each with the selector that
/// reaches it.
///
/// The selector is the whole point of the line, so it is first and it is in
/// backticks — a model copying an address out of prose is the failure mode this
/// format exists to prevent. `None` when the filter matched nothing, because an
/// empty heading reads as "the page has no buttons" when the truth is "your
/// filter excluded them"; the caller says which by leaving the section out and
/// letting the counts above stand.
fn render_addressable(items: &[Value], heading: &str, filter: Option<&str>) -> Option<Addressable> {
    let needle = filter.map(str::to_lowercase);
    let matching: Vec<&Value> = items
        .iter()
        .filter(|item| match &needle {
            None => true,
            Some(n) => ["selector", "label", "name", "id", "text", "type"]
                .iter()
                .filter_map(|k| str_at(item, k))
                .any(|v| v.to_lowercase().contains(n.as_str())),
        })
        .collect();
    if matching.is_empty() {
        return None;
    }

    let mut body = format!("### {heading} ({})\n\n", matching.len());
    for item in matching.iter().take(SELECTOR_BUDGET) {
        let selector = str_at(item, "selector").unwrap_or("(no selector)");
        let label = str_at(item, "label")
            .or_else(|| str_at(item, "name"))
            .unwrap_or("");
        let kind = str_at(item, "type").unwrap_or("");
        let required = item.get("required").and_then(Value::as_bool) == Some(true);
        let mut line = format!("- `{}`", one_line(selector));
        if !label.trim().is_empty() {
            line.push_str(&format!(" — {}", link_label(label)));
        }
        if !kind.is_empty() {
            line.push_str(&format!(" [{kind}]"));
        }
        if required {
            line.push_str(" (required)");
        }
        body.push_str(&line);
        body.push('\n');
    }
    let cut = (matching.len() > SELECTOR_BUDGET).then(|| {
        // No argument raises this one — the remedy is to narrow, not to widen —
        // so the notice names `selectors` rather than inventing a budget knob.
        format!(
            "{SELECTOR_BUDGET} of {} {} listed by a fixed per-category cap; narrow the list with \
             the `selectors` filter rather than re-reading for more",
            matching.len(),
            heading.to_lowercase()
        )
    });
    if cut.is_some() {
        body.push_str(&format!(
            "\n[truncated: {}]\n",
            cut.as_deref().unwrap_or_default()
        ));
    }
    body.push('\n');
    Some(Addressable { body, cut })
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

    /// The defaults `crate::fetch` applies, so these tests exercise the same
    /// numbers a real call would.
    fn limits() -> Limits {
        Limits::reading(50, 8_000)
    }

    fn render_default(v: &Value) -> Rendered {
        render(v, &limits()).unwrap()
    }

    /// Links enough to overrun a budget, with one in-content link appended so
    /// ordering is observable too.
    fn many_links(n: usize) -> Value {
        let mut links: Vec<Value> = (0..n)
            .map(|i| json!({ "text": format!("chrome {i}"), "href": "https://x/", "in_content": false }))
            .collect();
        links.push(json!({ "text": "the answer", "href": "https://a/", "in_content": true }));
        digest_with(
            "body",
            json!({ "digest": { "interactive": { "links": links } } }),
        )
    }

    #[test]
    fn an_empty_page_renders_as_a_result_not_a_complaint() {
        // The governing rule. If this ever renders as an error, "I could not
        // look" and "I looked and it was empty" have become the same message.
        let r = render(&digest_with("", json!({})), &limits()).expect("empty text is renderable");
        assert!(!r.truncated());
        assert!(r.truncation.is_none());
        assert!(r.markdown.contains("no readable text"), "{}", r.markdown);
        assert!(r.markdown.contains("HTTP 200"), "{}", r.markdown);
    }

    #[test]
    fn chromehands_own_truncation_is_passed_through() {
        let v = digest_with(
            "half a page",
            json!({ "digest": { "text_truncated": true, "text_chars_total": 90_000 } }),
        );
        let r = render_default(&v);
        assert!(
            r.truncated(),
            "chromehand cut the text and we reported whole"
        );
        assert!(r.markdown.contains("90000"), "{}", r.markdown);
    }

    /// The guarantee, on the axis that failed in the field: a cut names its
    /// own cap, the size of the loss, and the argument that undoes it. Delete
    /// any one of the three and this fails.
    #[test]
    fn a_cut_names_the_limit_the_loss_and_the_remedy() {
        let text = render_default(&digest_with(
            "half a page",
            json!({ "digest": { "text_truncated": true, "text_chars_total": 90_000 } }),
        ))
        .truncation
        .expect("text cut reported nothing");
        assert!(text.contains("max_chars=8000"), "no limit named: {text}");
        assert!(text.contains("90000"), "no size of loss: {text}");
        assert!(text.contains("max_chars=90000"), "no remedy: {text}");

        let links = render_default(&many_links(70))
            .truncation
            .expect("link cut reported nothing");
        assert!(links.contains("max_links=50"), "no limit named: {links}");
        assert!(links.contains("21 dropped"), "no size of loss: {links}");
        assert!(links.contains("max_links=71"), "no remedy: {links}");
    }

    /// The AP News shape exactly: prose well inside `max_chars`, an inventory
    /// past `max_links`. The bug was that this reported "truncated" with
    /// nothing to distinguish it from a cut article, which sent the reader to
    /// the one argument that would not have helped.
    #[test]
    fn a_link_cut_is_not_reported_as_a_text_cut() {
        let r = render_default(&many_links(70));
        let reason = r.truncation.expect("links were cut without saying so");
        assert!(reason.contains("links"), "{reason}");
        assert!(
            !reason.contains("page text"),
            "a link cut claimed the prose was cut: {reason}"
        );
        assert!(
            !reason.contains("max_chars"),
            "a link cut pointed at max_chars, which would change nothing: {reason}"
        );
    }

    /// Raising `max_links` past what the browser collected is futile, and the
    /// notice has to say so rather than advising a retry that returns the same
    /// page.
    #[test]
    fn a_link_cut_at_the_collector_ceiling_says_more_is_unreachable() {
        let r = render_default(&many_links(COLLECTOR_LINK_BUDGET));
        let reason = r.truncation.expect("links were cut without saying so");
        assert!(reason.contains("120"), "{reason}");
        assert!(reason.contains("never captured"), "{reason}");
    }

    /// Two caps that bind are two facts. Reporting only the first is how a
    /// reader raises `max_chars`, sees the prose return, and never learns the
    /// link list is still short.
    #[test]
    fn every_cap_that_bound_is_reported_not_only_the_first() {
        let mut v = many_links(70);
        merge(
            &mut v,
            json!({ "digest": { "text_truncated": true, "text_chars_total": 90_000 } }),
        );
        let reason = render_default(&v).truncation.expect("nothing reported");
        assert!(reason.contains("page text"), "{reason}");
        assert!(reason.contains("links"), "{reason}");
    }

    /// A cap with no argument behind it must not invent one. Advice the reader
    /// cannot follow is worse than the admission that there is none.
    #[test]
    fn a_cap_no_argument_raises_says_so_instead_of_naming_one() {
        let blob: Vec<Value> = (0..200)
            .map(|i| json!({ "@type": "Thing", "name": format!("padding value number {i}") }))
            .collect();
        let v = digest_with(
            "body",
            json!({ "digest": { "structured": { "json_ld": blob } } }),
        );
        let reason = render_default(&v).truncation.expect("JSON-LD cut silently");
        assert!(reason.contains("JSON-LD"), "{reason}");
        assert!(reason.contains("no argument raises"), "{reason}");
        assert!(
            !reason.contains("max_chars") && !reason.contains("max_links"),
            "named an argument that does not move this cap: {reason}"
        );
    }

    #[test]
    fn a_raised_link_budget_actually_returns_more_links() {
        // The remedy the notice advertises has to work. If `max_links` were
        // ignored the advice would be a lie that reads perfectly.
        let v = many_links(70);
        let wide = render(
            &v,
            &Limits {
                max_links: 71,
                ..limits()
            },
        )
        .unwrap();
        assert!(!wide.truncated(), "{:?}", wide.truncation);
        assert!(wide.markdown.contains("chrome 69"), "{}", wide.markdown);
    }

    #[test]
    fn a_blocked_page_says_so_before_the_prose() {
        let v = digest_with(
            "Verify you are human",
            json!({ "looks_blocked": true, "outcome": "blocked" }),
        );
        let r = render_default(&v);
        let warning = r.markdown.find("anti-bot challenge").expect("no warning");
        let content = r.markdown.find("## Content").expect("no content heading");
        assert!(warning < content, "the warning must precede the prose");
    }

    #[test]
    fn main_content_links_come_first_and_the_list_is_capped() {
        let r = render_default(&many_links(60));
        assert!(r.truncated(), "links were cut without saying so");
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
        let r = render_default(&v);
        assert!(r.markdown.contains("not observed"), "{}", r.markdown);
    }

    #[test]
    fn a_response_that_is_not_a_digest_is_a_failure() {
        let r = render(&json!({ "error": "browser", "detail": "nope" }), &limits());
        assert!(r.is_err(), "a non-digest rendered as a page");
    }
}

// endregion: Tests
